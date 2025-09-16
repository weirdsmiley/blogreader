use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
// MODIFIED: Add chrono for date formatting
use chrono::{DateTime, Utc};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    error::Error,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use feed_rs::parser as feed_parser;

// UNCHANGED: Feed, Manual, Config structs
#[derive(Debug, Deserialize, Clone)]
struct Feed {
    name: String,
    url: String,
}

#[derive(Debug, Deserialize, Clone)]
struct Manual {
    name: String,
    url: String,
}

#[derive(Debug, Deserialize, Clone)]
struct Config {
    feeds: Option<Vec<Feed>>,
    manual: Option<Vec<Manual>>,
}

// MODIFIED: Update enum to include post date
#[derive(Debug)]
enum Update {
    NewFeedItem(String, String, String, Option<DateTime<Utc>>), // blog name, title, link, date
    ManualUpdate(String, String),
    Error(String),
    Info(String),
}

type Cache = Arc<Mutex<HashMap<String, String>>>;

// MODIFIED: fetch_feed now extracts the post date
async fn fetch_feed(feed: Feed, tx: mpsc::Sender<Update>) {
    let response = match reqwest::get(&feed.url).await {
        Ok(res) => res,
        Err(e) => {
            let error_msg = format!("[ERROR] fetching {}: {}", feed.name, e);
            let _ = tx.send(Update::Error(error_msg)).await;
            return;
        }
    };

    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            let error_msg = format!("[ERROR] reading bytes for {}: {}", feed.name, e);
            let _ = tx.send(Update::Error(error_msg)).await;
            return;
        }
    };

    match feed_parser::parse(&bytes[..]) {
        Ok(parsed_feed) => {
            for entry in parsed_feed.entries.iter().take(5) {
                let title = entry.title.clone().map_or_else(|| "No Title".to_string(), |t| t.content);
                let link = entry.links.first().map_or("", |l| &l.href).to_string();
                // Extract the date - use updated as a fallback for published
                let date = entry.published.or(entry.updated);
                
                if let Err(e) = tx.send(Update::NewFeedItem(feed.name.clone(), title, link, date)).await {
                    eprintln!("Failed to send feed update: {}", e);
                    break;
                }
            }
        }
        Err(e) => {
            let error_msg = format!("[ERROR] parsing feed for {}: {}", feed.name, e);
            let _ = tx.send(Update::Error(error_msg)).await;
        }
    }
}

// UNCHANGED: check_manual_site, main
async fn check_manual_site(site: Manual, tx: mpsc::Sender<Update>, cache: Cache, cache_path: String) {
    let content = match reqwest::get(&site.url).await {
        Ok(res) => match res.text().await {
            Ok(text) => text,
            Err(e) => {
                let _ = tx.send(Update::Error(format!("[ERROR] reading content for {}: {}", site.name, e))).await;
                return;
            }
        },
        Err(e) => {
            let _ = tx.send(Update::Error(format!("Error fetching {}: {}", site.name, e))).await;
            return;
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let new_hash = format!("{:x}", hasher.finalize());

    let old_hash = {
        let cache_guard = cache.lock().unwrap();
        cache_guard.get(&site.url).cloned()
    };

    if old_hash.as_deref() != Some(&new_hash) {
        let update_message = format!("New content detected on {}", site.name);
        if let Err(e) = tx.send(Update::ManualUpdate(update_message, site.url.clone())).await {
            eprintln!("Failed to send manual update: {}", e);
        }

        {
            let mut cache_guard = cache.lock().unwrap();
            cache_guard.insert(site.url.clone(), new_hash);
        }

        let cache_content = {
            let cache_guard = cache.lock().unwrap();
            serde_json::to_string_pretty(&*cache_guard).unwrap()
        };
        
        if let Err(e) = tokio::fs::write(&cache_path, cache_content).await {
            eprintln!("Failed to write to cache file: {}", e);
        }
    } else {
        let _ = tx.send(Update::Info(format!("No changes for {}", site.name))).await;
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let res = run_app(&mut terminal).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err)
    }

    Ok(())
}

enum InputMode {
    Normal,
    Search,
}

// MODIFIED: App state now stores the formatted date string for each item
struct App {
    all_updates: Vec<(String, Option<String>, Option<String>, bool)>, // display_text, link, date_string, is_new
    info_messages: Vec<String>,
    list_state: ListState,
    input: String,
    input_mode: InputMode,
}

impl App {
    fn new(initial_updates: Vec<(String, Option<String>, Option<String>, bool)>) -> App {
        App {
            all_updates: initial_updates,
            info_messages: Vec::new(),
            list_state: ListState::default(),
            input: String::new(),
            input_mode: InputMode::Normal,
        }
    }

    fn first(&mut self, item_count: usize) {
        if item_count == 0 {
            self.list_state.select(None);
            return;
        }
        self.list_state.select(Some(0));
    }

    fn last(&mut self, item_count: usize) {
        if item_count == 0 {
            self.list_state.select(None);
            return;
        }
        self.list_state.select(Some(item_count - 1));
    }

    fn next(&mut self, item_count: usize) {
        if item_count == 0 {
            self.list_state.select(None);
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => if i >= item_count - 1 { 0 } else { i + 1 },
            None => 0,
        };
        self.list_state.select(Some(i));
    }
    
    fn previous(&mut self, item_count: usize) {
        if item_count == 0 {
            self.list_state.select(None);
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => if i == 0 { item_count - 1 } else { i - 1 },
            None => 0,
        };
        self.list_state.select(Some(i));
    }
}


async fn run_app<B: Backend>(terminal: &mut Terminal<B>) -> io::Result<()> {
    // MODIFIED: Initial updates tuple structure changed
    let initial_updates: Vec<(String, Option<String>, Option<String>, bool)> = vec![
        ("Press 'u' to check for updates.".to_string(), None, None, false),
        ("Press 'o' or Enter to open selected link.".to_string(), None, None, false),
        ("Press '/' to search/filter.".to_string(), None, None, false),
        ("Use j/k to scroll.".to_string(), None, None, false),
        ("Press g or G to go to first or last item.".to_string(), None, None, false),
        ("Press 'q' to quit.".to_string(), None, None, false),
    ];

    let mut app = App::new(initial_updates);
    app.list_state.select(Some(0));

    let (tx, mut rx) = mpsc::channel(100);

    let config_path = dirs::config_dir().unwrap().join("br/config.toml");

    let config: Config = match tokio::fs::read_to_string(&config_path).await {
        Ok(config_str) => toml::from_str(&config_str).unwrap_or(Config { feeds: None, manual: None }),
        Err(_) => {
            app.all_updates.push(("[ERROR] config.toml not found.".to_string(), None, None, false));
            Config { feeds: None, manual: None }
        }
    };
    
    let cache_path = dirs::data_dir().unwrap().join("br/cache.json").to_string_lossy().to_string();
    let cache_content = tokio::fs::read_to_string(&cache_path).await.unwrap_or_else(|_| "{}".to_string());
    let cache_map: HashMap<String, String> = serde_json::from_str(&cache_content).unwrap_or_default();
    let cache = Arc::new(Mutex::new(cache_map));

    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(250);

    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('/') => {
                            app.input_mode = InputMode::Search;
                        },
                        KeyCode::Char('g') => {
                             let filtered_count = app.all_updates.iter().filter(|(text, ..)| text.to_lowercase().contains(&app.input.to_lowercase())).count();
                             app.first(filtered_count);
                        },
                        KeyCode::Char('G') => {
                             let filtered_count = app.all_updates.iter().filter(|(text, ..)| text.to_lowercase().contains(&app.input.to_lowercase())).count();
                             app.last(filtered_count);
                        },
                        KeyCode::Char('j') => {
                             let filtered_count = app.all_updates.iter().filter(|(text, ..)| text.to_lowercase().contains(&app.input.to_lowercase())).count();
                             app.next(filtered_count);
                        },
                        KeyCode::Char('k') => {
                             let filtered_count = app.all_updates.iter().filter(|(text, ..)| text.to_lowercase().contains(&app.input.to_lowercase())).count();
                             app.previous(filtered_count);
                        },
                        KeyCode::Char('u') => {
                            for item in app.all_updates.iter_mut() {
                                item.3 = false;
                            }
                            app.all_updates.push(("Checking for updates...".to_string(), None, None, false));
                            app.list_state.select(Some(app.all_updates.len().saturating_sub(1)));
                            
                            if let Some(feeds) = config.feeds.clone() {
                                for feed in feeds {
                                    let tx_clone = tx.clone();
                                    tokio::spawn(fetch_feed(feed, tx_clone));
                                }
                            }
                            if let Some(manual_sites) = config.manual.clone() {
                                for site in manual_sites {
                                    let tx_clone = tx.clone();
                                    let cache_clone = cache.clone();
                                    let cache_path_clone = cache_path.clone();
                                    tokio::spawn(check_manual_site(site, tx_clone, cache_clone, cache_path_clone));
                                }
                            }
                        },
                        KeyCode::Char('o') | KeyCode::Enter => {
                            if let Some(selected_index) = app.list_state.selected() {
                                let filtered_updates: Vec<_> = app.all_updates.iter()
                                    .filter(|(text, ..)| text.to_lowercase().contains(&app.input.to_lowercase()))
                                    .collect();

                                if let Some((_, Some(link), _, _)) = filtered_updates.get(selected_index) {
                                    if !link.is_empty() {
                                        match open::that(link) {
                                            Ok(_) => { let _ = tx.try_send(Update::Info(format!("Opened {}", link))); },
                                            Err(e) => { let _ = tx.try_send(Update::Error(format!("Failed to open link: {}", e))); }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    },
                    InputMode::Search => match key.code {
                        KeyCode::Enter => {
                            app.input_mode = InputMode::Normal;
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                            app.input.clear();
                        }
                        _ => {}
                    },
                }
            }
        }

        if let Ok(update) = rx.try_recv() {
            match update {
                // MODIFIED: Handle the new date field from the update
                Update::NewFeedItem(blog_name, title, link, date) => {
                    let new_link = Some(link);
                    let is_duplicate = app.all_updates.iter().any(|(_, l, ..)| l == &new_link);
                    if !is_duplicate {
                        // Format the date into a string if it exists
                        let date_str = date.map(|dt| dt.format("%e %b %y").to_string());
                        
                        // Create the final display text including the date
                        let display_text = if let Some(d) = &date_str {
                            format!("[FEED] {} | {:<20} | {}", d, blog_name, title)
                        } else {
                            format!("[FEED] {:<32} | {}", blog_name, title)
                        };
                        
                        app.all_updates.push((display_text, new_link, date_str, true));
                    }
                }
                Update::ManualUpdate(message, link) => {
                    let new_link = Some(link);
                    let is_duplicate = app.all_updates.iter().any(|(_, l, ..)| l == &new_link);
                    if !is_duplicate {
                        app.all_updates.push((format!("[MANUAL] {}", message), new_link, None, true));
                    }
                }
                Update::Error(e) => {
                    app.all_updates.push((format!("[ERROR] {}", e), None, None, false));
                }
                Update::Info(msg) => {
                    app.info_messages.push(format!("[INFO] {}", msg));
                    if app.info_messages.len() > 5 {
                        app.info_messages.remove(0);
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
}


// MODIFIED: UI function now correctly unpacks the updated tuple
fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Min(0),
                Constraint::Length(3),
                Constraint::Length(7),
            ]
            .as_ref(),
        )
        .split(f.size());
        
    let updates: Vec<_> = app.all_updates
        .iter()
        .filter(|(text, ..)| text.to_lowercase().contains(&app.input.to_lowercase()))
        .collect();
    
    if let Some(selected) = app.list_state.selected() {
        if selected >= updates.len() {
            app.list_state.select(Some(updates.len().saturating_sub(1)));
        }
    }

    let items: Vec<ListItem> = updates
        .iter()
        .map(|(text, _, _, is_new)| { // Unpack the new tuple
            let is_article = text.starts_with("[FEED]") || text.starts_with("[MANUAL]");
            
            let base_color = if text.starts_with("[FEED]") {
                Color::Cyan
            } else if text.starts_with("[MANUAL]") {
                Color::Yellow
            } else if text.starts_with("[ERROR]") {
                Color::Red
            } else if text.starts_with("Checking") {
                Color::Magenta
            } else {
                Color::White
            };

            let style = if is_article {
                if *is_new { // Dereference the borrowed bool
                    Style::default().fg(base_color)
                } else {
                    Style::default().fg(Color::Gray)
                }
            } else {
                Style::default().fg(base_color)
            };

            ListItem::new(text.clone()).style(style)
        })
        .collect();
        
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Blog Updates")
                .border_style(Style::default().fg(Color::White)),
        )
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol(">> ");

    f.render_stateful_widget(list, chunks[0], &mut app.list_state);
    
    let search_bar = Paragraph::new(app.input.as_str())
        .style(match app.input_mode {
            InputMode::Normal => Style::default(),
            InputMode::Search => Style::default().fg(Color::Yellow),
        })
        .block(Block::default().borders(Borders::ALL).title("Search"));
    f.render_widget(search_bar, chunks[1]);
    
    if let InputMode::Search = app.input_mode {
        f.set_cursor(
            chunks[1].x + app.input.len() as u16 + 1,
            chunks[1].y + 1,
        )
    }

    let info_items: Vec<ListItem> = app.info_messages
        .iter()
        .map(|msg| ListItem::new(msg.clone()).style(Style::default().fg(Color::Green)))
        .collect();

    let info_list = List::new(info_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Info")
            .border_style(Style::default().fg(Color::Green)),
    );

    f.render_widget(info_list, chunks[2]);
}
