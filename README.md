# blog reader - br


## ⚠️ Attention

This is vibe coded.


## Installation

```bash
./install.sh
```

This will create `~/.config/br/` and `~/.local/share/br/` directories. For
example purposes, I have provided my own configuration file
`config.example.toml`. It contains a list of rss/atom feeds of blogs that I
follow personally.

## Usage

```bash
$ br

>> Press 'u' to check for updates.
   Press 'o' or Enter to open selected link.
   Use j/k to scroll.
   Press 'q' to quit.
```

## Configuration

There are two types of configurations: `[[feeds]]` and `[[manual]]`. Both
require two variables: `name` and `url`.

`[[feeds]]` refer to either rss or atom feed. `[[manual]]` keeps track of
websites which don't have either of these.

To add a hacker news feed, open `~/.config/br/config.toml` and put

```bash
[[feeds]]
name = "Hacker News"
url  = "https://news.ycombinator.com/rss"
```

Similarly for manually tracking, put

```bash
[[manual]]
name = "Hacker News"
url  = "https://news.ycombinator.com"
```

The manual tracker will only check against a previous hash. It can only
_suggest_ if new posts may have been posted. Use `[[feeds]]` method for better
results.

### Tips

To figure out if a website provides any feed for its blogs, use

```bash
wget -qO- https://example.com/ | grep -iE 'rss|atom'
```

And copy the url from `href` tag.
