use chrono::{DateTime, Utc};
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use nzbg_core::models::DupMode;

use crate::{FeedItemInfo, FeedItemStatus};

#[derive(Debug, Default)]
struct ItemBuilder {
    title: String,
    url: String,
    description: String,
    category: String,
    pub_date: String,
    size: u64,
    imdb_id: Option<String>,
    rageid: Option<u32>,
    tvdbid: Option<u32>,
    season: Option<String>,
    episode: Option<String>,
}

impl ItemBuilder {
    fn build(self) -> FeedItemInfo {
        let time = parse_rfc2822_or_now(&self.pub_date);
        FeedItemInfo {
            title: self.title,
            url: self.url,
            filename: String::new(),
            category: self.category,
            size: self.size,
            time,
            description: self.description,
            season: self.season,
            episode: self.episode,
            imdb_id: self.imdb_id,
            rageid: self.rageid,
            tvdbid: self.tvdbid,
            tvmazeid: None,
            rating: None,
            genres: Vec::new(),
            tags: Vec::new(),
            dupe_key: String::new(),
            dupe_score: 0,
            dupe_mode: DupMode::Score,
            status: FeedItemStatus::Unknown,
            added_nzb_id: None,
        }
    }
}

fn parse_rfc2822_or_now(s: &str) -> DateTime<Utc> {
    if s.is_empty() {
        return Utc::now();
    }
    DateTime::parse_from_rfc2822(s)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| DateTime::parse_from_rfc3339(s).map(|dt| dt.with_timezone(&Utc)))
        .unwrap_or_else(|_| Utc::now())
}

fn get_attr(e: &BytesStart, name: &[u8]) -> Option<String> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == name {
            return attr.unescape_value().ok().map(|s| s.into_owned());
        }
    }
    None
}

#[derive(Debug, PartialEq)]
enum ParseState {
    Root,
    InItem,
    InItemTag(String),
}

pub fn parse_feed(data: &[u8]) -> Result<Vec<FeedItemInfo>, anyhow::Error> {
    let mut reader = Reader::from_reader(data);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::with_capacity(4096);
    let mut state = ParseState::Root;
    let mut items = Vec::new();
    let mut current: Option<ItemBuilder> = None;
    let mut text_buf = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let tag_bytes = e.name().as_ref().to_vec();
                let local = local_name(&tag_bytes);
                handle_open(
                    &tag_bytes,
                    local,
                    e,
                    false,
                    &mut state,
                    &mut current,
                    &mut text_buf,
                );
            }
            Ok(Event::Empty(ref e)) => {
                let tag_bytes = e.name().as_ref().to_vec();
                let local = local_name(&tag_bytes);
                handle_open(
                    &tag_bytes,
                    local,
                    e,
                    true,
                    &mut state,
                    &mut current,
                    &mut text_buf,
                );
            }
            Ok(Event::Text(ref e)) => {
                if matches!(&state, ParseState::InItemTag(_))
                    && let Ok(t) = e.unescape()
                {
                    text_buf.push_str(&t);
                }
            }
            Ok(Event::CData(ref e)) => {
                if matches!(&state, ParseState::InItemTag(_))
                    && let Ok(t) = std::str::from_utf8(e.as_ref())
                {
                    text_buf.push_str(t);
                }
            }
            Ok(Event::End(ref e)) => {
                let tag_bytes = e.name().as_ref().to_vec();
                let local = local_name(&tag_bytes);

                match local {
                    b"item" | b"entry" => {
                        if let Some(builder) = current.take() {
                            items.push(builder.build());
                        }
                        state = ParseState::Root;
                    }
                    _ => {
                        if let ParseState::InItemTag(ref tag_name) = state {
                            let tag_local = local_name(tag_name.as_bytes());
                            if let Some(item) = current.as_mut() {
                                match tag_local {
                                    b"title" => item.title.clone_from(&text_buf),
                                    b"link" => {
                                        if item.url.is_empty() {
                                            item.url.clone_from(&text_buf);
                                        }
                                    }
                                    b"description" | b"summary" => {
                                        item.description.clone_from(&text_buf);
                                    }
                                    b"category" => {
                                        item.category.clone_from(&text_buf);
                                    }
                                    b"pubDate" | b"published" | b"updated" => {
                                        item.pub_date.clone_from(&text_buf);
                                    }
                                    _ => {}
                                }
                            }
                            state = ParseState::InItem;
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(anyhow::anyhow!("XML parse error: {e}")),
            _ => {}
        }
        buf.clear();
    }

    Ok(items)
}

fn handle_open(
    tag_bytes: &[u8],
    local: &[u8],
    e: &BytesStart,
    is_empty: bool,
    state: &mut ParseState,
    current: &mut Option<ItemBuilder>,
    text_buf: &mut String,
) {
    match (&*state, local) {
        (ParseState::Root, b"item" | b"entry") => {
            *state = ParseState::InItem;
            *current = Some(ItemBuilder::default());
        }
        (ParseState::InItem, b"enclosure") => {
            if let Some(item) = current.as_mut()
                && let Some(url) = get_attr(e, b"url")
            {
                item.url = url;
            }
        }
        (ParseState::InItem, b"link") => {
            if let Some(item) = current.as_mut()
                && let Some(href) = get_attr(e, b"href")
                && item.url.is_empty()
            {
                item.url = href;
            }
            if !is_empty {
                let tag_name = String::from_utf8_lossy(tag_bytes).to_string();
                *state = ParseState::InItemTag(tag_name);
                text_buf.clear();
            }
        }
        (ParseState::InItem, _) if is_newznab_attr(tag_bytes) => {
            if let Some(item) = current.as_mut() {
                handle_newznab_attr(e, item);
            }
        }
        (ParseState::InItem, _) => {
            if !is_empty {
                let tag_name = String::from_utf8_lossy(tag_bytes).to_string();
                *state = ParseState::InItemTag(tag_name);
                text_buf.clear();
            }
        }
        _ => {}
    }
}

fn local_name(tag: &[u8]) -> &[u8] {
    tag.iter()
        .position(|&b| b == b':')
        .map_or(tag, |pos| &tag[pos + 1..])
}

fn is_newznab_attr(tag: &[u8]) -> bool {
    let s = String::from_utf8_lossy(tag);
    (s.contains("newznab:") || s.contains("nZEDb:")) && local_name(tag) == b"attr"
}

fn handle_newznab_attr(e: &BytesStart, item: &mut ItemBuilder) {
    let name = get_attr(e, b"name").unwrap_or_default();
    let value = get_attr(e, b"value").unwrap_or_default();

    match name.as_str() {
        "size" => {
            if let Ok(v) = value.parse::<u64>() {
                item.size = v;
            }
        }
        "imdb" => item.imdb_id = Some(format!("tt{value}")),
        "rageid" => {
            if let Ok(v) = value.parse::<u32>() {
                item.rageid = Some(v);
            }
        }
        "tvdbid" => {
            if let Ok(v) = value.parse::<u32>() {
                item.tvdbid = Some(v);
            }
        }
        "season" => item.season = Some(value),
        "episode" => item.episode = Some(value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS_SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
  <channel>
    <title>Test Feed</title>
    <item>
      <title>Show.S01E02.720p</title>
      <link>https://example.com/details/123</link>
      <description>A great show</description>
      <category>TV &gt; HD</category>
      <enclosure url="https://example.com/getnzb/123.nzb" length="735000000" type="application/x-nzb"/>
      <pubDate>Sat, 01 Jan 2022 12:00:00 +0000</pubDate>
    </item>
    <item>
      <title>Movie.2022.1080p</title>
      <link>https://example.com/details/456</link>
      <description>A great movie</description>
      <category>Movies</category>
      <enclosure url="https://example.com/getnzb/456.nzb" length="1500000000" type="application/x-nzb"/>
      <pubDate>Sun, 02 Jan 2022 15:30:00 +0000</pubDate>
    </item>
  </channel>
</rss>"#;

    const NEWZNAB_SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
  <channel>
    <title>Newznab Feed</title>
    <item>
      <title>Show.S03E05.1080p</title>
      <link>https://example.com/details/789</link>
      <enclosure url="https://example.com/getnzb/789.nzb" length="500000000" type="application/x-nzb"/>
      <newznab:attr name="size" value="524288000"/>
      <newznab:attr name="imdb" value="7654321"/>
      <newznab:attr name="rageid" value="42"/>
      <newznab:attr name="tvdbid" value="999"/>
      <newznab:attr name="season" value="3"/>
      <newznab:attr name="episode" value="5"/>
    </item>
  </channel>
</rss>"#;

    const ATOM_SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Atom Feed</title>
  <entry>
    <title>Atom.Entry.Test</title>
    <link href="https://example.com/atom/1"/>
    <summary>An atom entry</summary>
    <published>2022-01-15T10:00:00Z</published>
  </entry>
</feed>"#;

    #[test]
    fn parse_rss_items() {
        let items = parse_feed(RSS_SAMPLE.as_bytes()).unwrap();
        assert_eq!(items.len(), 2);

        assert_eq!(items[0].title, "Show.S01E02.720p");
        assert_eq!(items[0].url, "https://example.com/getnzb/123.nzb");
        assert_eq!(items[0].description, "A great show");
        assert_eq!(items[0].category, "TV > HD");

        assert_eq!(items[1].title, "Movie.2022.1080p");
        assert_eq!(items[1].url, "https://example.com/getnzb/456.nzb");
    }

    #[test]
    fn parse_newznab_attributes() {
        let items = parse_feed(NEWZNAB_SAMPLE.as_bytes()).unwrap();
        assert_eq!(items.len(), 1);

        let item = &items[0];
        assert_eq!(item.title, "Show.S03E05.1080p");
        assert_eq!(item.size, 524_288_000);
        assert_eq!(item.imdb_id.as_deref(), Some("tt7654321"));
        assert_eq!(item.rageid, Some(42));
        assert_eq!(item.tvdbid, Some(999));
        assert_eq!(item.season.as_deref(), Some("3"));
        assert_eq!(item.episode.as_deref(), Some("5"));
    }

    #[test]
    fn parse_atom_entry() {
        let items = parse_feed(ATOM_SAMPLE.as_bytes()).unwrap();
        assert_eq!(items.len(), 1);

        let item = &items[0];
        assert_eq!(item.title, "Atom.Entry.Test");
        assert_eq!(item.url, "https://example.com/atom/1");
        assert_eq!(item.description, "An atom entry");
    }

    #[test]
    fn parse_empty_feed() {
        let xml = r#"<?xml version="1.0"?><rss><channel></channel></rss>"#;
        let items = parse_feed(xml.as_bytes()).unwrap();
        assert!(items.is_empty());
    }
}
