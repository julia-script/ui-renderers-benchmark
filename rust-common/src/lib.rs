//! Shared chat dataset loader, assets, colors, and streaming constants for GPUI, Rune, and FerroUI.

pub const WIDTH: f32 = 1040.0;
pub const HEIGHT: f32 = 720.0;
pub const INITIAL_MESSAGES: usize = 1000;
pub const SCROLL_FRAMES: usize = 30;
pub const STREAM_FRAMES: usize = 30;
pub const SCROLL_STEP: f32 = 480.0;

pub const CHAT_TXT: &str = include_str!("../../data/chat.txt");

pub const INTER_REGULAR: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
pub const INTER_BOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-Bold.ttf");
pub const INTER_ITALIC: &[u8] = include_bytes!("../../assets/fonts/Inter-Italic.ttf");
pub const MONO: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

pub const PICTURES: &[&[u8]; 4] = &[
    include_bytes!("../../assets/images/picture-0.png"),
    include_bytes!("../../assets/images/picture-1.png"),
    include_bytes!("../../assets/images/picture-2.png"),
    include_bytes!("../../assets/images/picture-3.png"),
];

pub const STREAM_WORDS: &[&str; 24] = &[
    "incremental", "layout", "shaping", "glyph", "pipeline", "compositor",
    "frame", "budget", "viewport", "scroller", "harfbuzz", "rasterizer",
    "surface", "texture", "quad", "atlas", "baseline", "kerning",
    "measured", "cached", "retained", "painted", "submitted", "aligned",
];

pub mod colors {
    pub const BG: u32 = 0xf6f7f9;
    pub const TEXT: u32 = 0x1f2430;
    pub const HEADING: u32 = 0x111827;
    pub const AUTHOR: u32 = 0x16181d;
    pub const MUTED: u32 = 0x6b7280;
    pub const CARD: u32 = 0xffffff;
    pub const CARD_BORDER: u32 = 0xe1e5eb;
    pub const MINE_CARD: u32 = 0xeef4ff;
    pub const MINE_BORDER: u32 = 0xc8dcff;
    pub const AVATAR_AI: u32 = 0x2563eb;
    pub const AVATAR_MINE: u32 = 0x0f766e;
    pub const WHITE: u32 = 0xffffff;
    pub const TAG_BG: u32 = 0xeef0f4;
    pub const TAG_TEXT: u32 = 0x4b5563;
    pub const LINK: u32 = 0x2563eb;
    pub const QUOTE_LINE: u32 = 0x93c5fd;
    pub const QUOTE_TEXT: u32 = 0x4b5563;
    pub const PRE_BG: u32 = 0x141820;
    pub const PRE_TEXT: u32 = 0xe5e7eb;
    pub const PRE_BORDER: u32 = 0x2b3240;
    pub const TH_BG: u32 = 0xf1f3f7;
    pub const TD_ODD: u32 = 0xfafbfc;
    pub const TD_BORDER: u32 = 0xeceff4;
    pub const ACT_BG: u32 = 0xf1f3f7;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    None,
    Bold,
    Italic,
    Code,
    Link,
}

#[derive(Clone, Debug)]
pub struct Span {
    pub mark: Mark,
    pub text: String,
}

#[derive(Clone, Debug)]
pub enum Block {
    P(Vec<Span>),
    H(String),
    Q(Vec<Span>),
    Ul(Vec<Vec<Span>>),
    Pre(Vec<String>),
    Table { head: Vec<String>, rows: Vec<Vec<String>> },
    Img { picture: usize, width: f32, height: f32, caption: String },
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: usize,
    pub version: u64,
    pub mine: bool,
    pub author: String,
    pub initials: String,
    pub time: String,
    pub tags: Vec<String>,
    pub blocks: Vec<Block>,
}

fn parse_spans(s: &str) -> Vec<Span> {
    let mut out = Vec::new();
    for part in s.split('\t') {
        if part.len() >= 2 && part.as_bytes()[1] == b':' {
            let mark = match part.as_bytes()[0] {
                b'B' => Mark::Bold,
                b'I' => Mark::Italic,
                b'C' => Mark::Code,
                b'L' => Mark::Link,
                _ => Mark::None,
            };
            out.push(Span { mark, text: part[2..].to_string() });
        }
    }
    out
}

pub fn parse_corpus() -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::with_capacity(INITIAL_MESSAGES + 20);
    for line in CHAT_TXT.lines() {
        if let Some(rest) = line.strip_prefix("M|") {
            let parts: Vec<&str> = rest.split('|').collect();
            let id: usize = parts[0].parse().unwrap_or(0);
            let mine = parts[1] == "1";
            let author = parts[2].to_string();
            let initials = parts[3].to_string();
            let time = parts[4].to_string();
            let tags: Vec<String> = parts[5].split(',').filter(|t| !t.is_empty()).map(|s| s.to_string()).collect();
            messages.push(Message { id, version: 0, mine, author, initials, time, tags, blocks: Vec::new() });
        } else if let Some(msg) = messages.last_mut() {
            if let Some(rest) = line.strip_prefix("P|") {
                msg.blocks.push(Block::P(parse_spans(rest)));
            } else if let Some(rest) = line.strip_prefix("H|") {
                msg.blocks.push(Block::H(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix("Q|") {
                msg.blocks.push(Block::Q(parse_spans(rest)));
            } else if let Some(rest) = line.strip_prefix("L|") {
                let spans = parse_spans(rest);
                if let Some(Block::Ul(items)) = msg.blocks.last_mut() {
                    items.push(spans);
                } else {
                    msg.blocks.push(Block::Ul(vec![spans]));
                }
            } else if let Some(rest) = line.strip_prefix("C|") {
                msg.blocks.push(Block::Pre(rest.split('\t').map(|s| s.to_string()).collect()));
            } else if let Some(rest) = line.strip_prefix("TH|") {
                msg.blocks.push(Block::Table {
                    head: rest.split('\t').map(|s| s.to_string()).collect(),
                    rows: Vec::new(),
                });
            } else if let Some(rest) = line.strip_prefix("TR|") {
                if let Some(Block::Table { rows, .. }) = msg.blocks.last_mut() {
                    rows.push(rest.split('\t').map(|s| s.to_string()).collect());
                }
            } else if let Some(rest) = line.strip_prefix("I|") {
                let parts: Vec<&str> = rest.splitn(4, '|').collect();
                msg.blocks.push(Block::Img {
                    picture: parts[0].parse().unwrap_or(0),
                    width: parts[1].parse().unwrap_or(360.0),
                    height: parts[2].parse().unwrap_or(202.0),
                    caption: parts[3].to_string(),
                });
            }
        }
    }
    messages
}

pub fn make_stream_message(id: usize) -> Message {
    Message {
        id,
        version: 0,
        mine: false,
        author: "Assistant".into(),
        initials: "A".into(),
        time: "21:00".into(),
        tags: vec!["streaming".into()],
        blocks: vec![Block::P(vec![Span { mark: Mark::None, text: String::new() }])],
    }
}

/// Apply one streaming frame update to `messages`, matching Chromium and SilkUI step for step.
pub fn apply_stream_step(
    messages: &mut Vec<Message>,
    extra: &[Message],
    next_extra: &mut usize,
    stream_frame: usize,
    stream_id_counter: &mut usize,
) {
    // Append 6 words to the last paragraph of the last message
    if let Some(last) = messages.last_mut() {
        last.version += 1;
        if let Some(Block::P(spans)) = last.blocks.last_mut() {
            if let Some(sp) = spans.last_mut() {
                for w in 0..6 {
                    if !sp.text.is_empty() {
                        sp.text.push(' ');
                    }
                    sp.text.push_str(STREAM_WORDS[(stream_frame * 6 + w) % STREAM_WORDS.len()]);
                }
            }
        }
        if stream_frame == 10 {
            last.blocks.push(Block::P(vec![Span { mark: Mark::None, text: String::new() }]));
        }
        if stream_frame == 22 {
            last.blocks.push(Block::Img {
                picture: 1,
                width: 360.0,
                height: 202.0,
                caption: "Figure live: streamed chart during active response.".into(),
            });
            last.blocks.push(Block::P(vec![Span { mark: Mark::None, text: String::new() }]));
        }
    }
    if stream_frame == 15 {
        if *next_extra < extra.len() {
            messages.push(extra[*next_extra].clone());
            *next_extra += 1;
        }
        if *next_extra < extra.len() {
            messages.push(extra[*next_extra].clone());
            *next_extra += 1;
        }
        *stream_id_counter += 1;
        messages.push(make_stream_message(*stream_id_counter));
    }
}
