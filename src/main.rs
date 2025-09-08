use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame, Terminal,
};
use serde::{Deserialize};
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

#[derive(Debug)]
enum Update {
    NewFeedItem(String, String, String), // blog name, title, link
    ManualUpdate(String, String),
    Error(String),
    Info(String),
}

type Cache = Arc<Mutex<HashMap<String, String>>>;

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
                if let Err(e) = tx.send(Update::NewFeedItem(feed.name.clone(), title, link)).await {
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

async fn run_app<B: Backend>(terminal: &mut Terminal<B>) -> io::Result<()> {
    // Each item is a tuple: (display_text, link, is_new)
    let mut main_updates: Vec<(String, Option<String>, bool)> = vec![
        ("Press 'u' to check for updates.".to_string(), None, false),
        ("Press 'o' or Enter to open selected link.".to_string(), None, false),
        ("Use j/k to scroll.".to_string(), None, false),
        ("Press 'q' to quit.".to_string(), None, false),
    ];
    let mut info_messages: Vec<String> = Vec::new();

    let mut list_state = ListState::default();
    list_state.select(Some(0));

    let (tx, mut rx) = mpsc::channel(100);

    let config_path = dirs::config_dir().unwrap().join("br/config.toml");

    let config: Config = match tokio::fs::read_to_string(&config_path).await {
        Ok(config_str) => toml::from_str(&config_str).unwrap_or(Config { feeds: None, manual: None }),
        Err(_) => {
            main_updates.push(("[ERROR] config.toml not found.".to_string(), None, false));
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
        terminal.draw(|f| ui(f, &main_updates, &info_messages, &mut list_state))?;

        let timeout = tick_rate.checked_sub(last_tick.elapsed()).unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => return Ok(()),
                    KeyCode::Char('u') => {
                        // Mark all existing articles as old
                        for item in main_updates.iter_mut() {
                            item.2 = false;
                        }
                        main_updates.push(("Checking for updates...".to_string(), None, false));
                        list_state.select(Some(main_updates.len().saturating_sub(1)));
                        
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
                        if let Some(selected_index) = list_state.selected() {
                            if let Some((_, Some(link), _)) = main_updates.get(selected_index) {
                                if !link.is_empty() {
                                    match open::that(link) {
                                        Ok(_) => { let _ = tx.try_send(Update::Info(format!("Opened {}", link))); },
                                        Err(e) => { let _ = tx.try_send(Update::Error(format!("Failed to open link: {}", e))); }
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Char('j') => {
                        let i = match list_state.selected() {
                            Some(i) => if i >= main_updates.len() - 1 { 0 } else { i + 1 },
                            None => 0,
                        };
                        list_state.select(Some(i));
                    }
                    KeyCode::Char('k') => {
                        let i = match list_state.selected() {
                            Some(i) => if i == 0 { main_updates.len() - 1 } else { i - 1 },
                            None => 0,
                        };
                        list_state.select(Some(i));
                    }
                    _ => {}
                }
            }
        }

        if let Ok(update) = rx.try_recv() {
            match update {
                Update::NewFeedItem(blog_name, title, link) => {
                    let new_link = Some(link);
                    let is_duplicate = main_updates.iter().any(|(_, l, _)| l == &new_link);
                    if !is_duplicate {
                        main_updates.push((format!("[FEED] {:<30} | {}", blog_name, title), new_link, true));
                    }
                }
                Update::ManualUpdate(message, link) => {
                     let new_link = Some(link);
                    let is_duplicate = main_updates.iter().any(|(_, l, _)| l == &new_link);
                    if !is_duplicate {
                        main_updates.push((format!("[MANUAL] {}", message), new_link, true));
                    }
                }
                Update::Error(e) => {
                    main_updates.push((format!("[ERROR] {}", e), None, false));
                }
                Update::Info(msg) => {
                    info_messages.push(format!("[INFO] {}", msg));
                    if info_messages.len() > 5 {
                        info_messages.remove(0);
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
}

fn ui(f: &mut Frame, updates: &[(String, Option<String>, bool)], info: &[String], state: &mut ListState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(0), Constraint::Length(7)].as_ref())
        .split(f.size());

    let items: Vec<ListItem> = updates
        .iter()
        .map(|(text, _, is_new)| {
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
                Color::White // System messages
            };

            let style = if is_article {
                if *is_new {
                    Style::default().fg(base_color) // Bright for new
                } else {
                    Style::default().fg(Color::Gray) // Dim for old
                }
            } else {
                Style::default().fg(base_color) // Normal for system messages
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

    f.render_stateful_widget(list, chunks[0], state);

    let info_items: Vec<ListItem> = info
        .iter()
        .map(|msg| ListItem::new(msg.clone()).style(Style::default().fg(Color::Green)))
        .collect();

    let info_list = List::new(info_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Info")
            .border_style(Style::default().fg(Color::Green)),
    );

    f.render_widget(info_list, chunks[1]);
}

