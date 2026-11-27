//! Rune benchmark on the shared simulated chat session (500 messages + scroll + stream).
//! Uses Rune's built-in `frame-profile` (`rune::take_frame_phases`) to separate:
//!   - view / commit / apply (phases[0], phases[1] - phases[2], phases[5])
//!   - text shaping & line breaking (phases[3])
//!   - pure layout (phases[2] - phases[3])
//!   - paint + CPU glyph mask rasterization (phases[4])

use std::sync::Arc;
use std::time::Instant;

use chat_common::{
    Block, HEIGHT, INITIAL_MESSAGES, INTER_BOLD, INTER_ITALIC, INTER_REGULAR, MONO, Mark, Message,
    PICTURES, SCROLL_FRAMES, SCROLL_STEP, STREAM_FRAMES, Span, WIDTH, apply_stream_step,
    colors as c, make_stream_message, parse_corpus,
};
use rune::text::TextSystem;
use rune::{
    Color, Cx, El, Env, Family, Image, ImageCache, Runtime, ScrollSpec, Span as RSpan, TextStyle,
    col, el, memo, rich, row, spacer, take_frame_phases, text,
};

const FONTS: &[&[u8]] = &[INTER_REGULAR, INTER_BOLD, INTER_ITALIC, MONO];
const FAMILIES: (&str, &str) = ("Inter", "JetBrains Mono");

struct State {
    messages: Vec<Message>,
    pictures: Vec<Image>,
    _cache: Option<ImageCache>,
}

fn rgb(hex: u32) -> Color {
    rune::color::hex(&format!("#{hex:06x}")).expect("color")
}

const fn sans(size: f32, weight: f32, line: f32) -> TextStyle {
    TextStyle::new(size, weight).lh(line)
}

const fn mono(size: f32, line: f32) -> TextStyle {
    TextStyle::new(size, 400.0).family(Family::Mono).lh(line)
}

fn inline(spans: &[Span], style: TextStyle, color: Color, italic: bool) -> El {
    let mut content = String::new();
    let mut ranges = Vec::new();
    for s in spans {
        let start = content.len();
        content.push_str(&s.text);
        let end = content.len();
        let range = match s.mark {
            Mark::None => None,
            Mark::Bold => Some(RSpan::new(start, end).weight(700.0)),
            Mark::Italic => Some(RSpan::new(start, end).italic()),
            Mark::Code => Some(RSpan::new(start, end).mono().size(12.5).bg(rgb(c::TAG_BG), 4.0)),
            Mark::Link => Some(RSpan::new(start, end).color(rgb(c::LINK))),
        };
        match (range, italic) {
            (Some(r), true) => ranges.push(r.italic()),
            (Some(r), false) => ranges.push(r),
            (None, true) => ranges.push(RSpan::new(start, end).italic()),
            (None, false) => {}
        }
    }
    rich(content, ranges).font(style).color(color)
}

fn label(content: &str, style: TextStyle, color: Color) -> El {
    text(content).font(style).color(color)
}

fn block(b: &Block, pictures: &[Image]) -> El {
    let ink = rgb(c::TEXT);
    match b {
        Block::P(spans) => inline(spans, sans(14.0, 400.0, 21.0), ink, false),
        Block::Q(spans) => inline(spans, sans(14.0, 400.0, 21.0), rgb(c::QUOTE_TEXT), true)
            .pl(12.0)
            .border_sides([0.0, 0.0, 0.0, 3.0], rgb(c::QUOTE_LINE))
            .border_layout(),
        Block::H(content) => label(content, sans(16.0, 700.0, 22.0), rgb(c::HEADING)).wrap(),
        Block::Ul(items) => col().gap(4.0).pl(4.0).children(items.iter().map(|spans| {
            row()
                .gap(8.0)
                .child(el().size(6.0, 6.0).mt(8.0).radius(3.0).fill(rgb(c::MUTED)))
                .child(inline(spans, sans(14.0, 400.0, 20.0), ink, false).grow(1.0).min_w(0.0))
        })),
        Block::Pre(lines) => col()
            .px(12.0)
            .py(10.0)
            .fill(rgb(c::PRE_BG))
            .border(1.0, rgb(c::PRE_BORDER))
            .radius(6.0)
            .border_layout()
            .children(lines.iter().map(|l| label(l, mono(12.5, 18.0), rgb(c::PRE_TEXT)))),
        Block::Table { head, rows } => {
            let n = head.len();
            let total = (n + 1) as f32;
            let cell = |content: &str, column: usize, header: bool, odd: bool| {
                let style = sans(13.0, if header { 700.0 } else { 400.0 }, 18.0);
                let mut e = label(content, style, if header { rgb(c::HEADING) } else { ink })
                    .truncate()
                    .px(8.0)
                    .py(5.0)
                    .border_sides(
                        [0.0, 0.0, 1.0, 0.0],
                        rgb(if header { c::CARD_BORDER } else { c::TD_BORDER }),
                    )
                    .border_layout()
                    .w_frac(if column == 0 { 2.0 / total } else { 1.0 / total } - 1e-6);
                if header {
                    e = e.fill(rgb(c::TH_BG));
                } else if odd {
                    e = e.fill(rgb(c::TD_ODD));
                }
                e
            };
            let mut cells = Vec::with_capacity(head.len() * (rows.len() + 1));
            for (i, h) in head.iter().enumerate() {
                cells.push(cell(h, i, true, false));
            }
            for (r, rv) in rows.iter().enumerate() {
                let odd = r % 2 == 1;
                for (i, v) in rv.iter().enumerate() {
                    cells.push(cell(v, i, false, odd));
                }
            }
            row()
                .flex_wrap()
                .border(1.0, rgb(c::CARD_BORDER))
                .radius(6.0)
                .border_layout()
                .children(cells)
        }
        Block::Img { picture, width, height, caption } => col()
            .gap(6.0)
            .items_start()
            .child(el().size(*width, *height).radius(8.0).pixels(pictures[*picture % 4].pixels()))
            .child(label(caption, sans(12.0, 400.0, 16.0), rgb(c::MUTED)).wrap()),
    }
}

fn msg_view(m: &Message, pictures: &[Image]) -> El {
    let av_bg = if m.mine { c::AVATAR_MINE } else { c::AVATAR_AI };
    let head = row()
        .items_center()
        .gap(10.0)
        .child(
            el().size(30.0, 30.0)
                .radius(15.0)
                .fill(rgb(av_bg))
                .center()
                .child(label(&m.initials, sans(12.0, 700.0, 16.0), rgb(c::WHITE))),
        )
        .child(
            col()
                .child(label(&m.author, sans(14.0, 700.0, 18.0), rgb(c::AUTHOR)))
                .child(label(&m.time, sans(12.0, 400.0, 16.0), rgb(c::MUTED))),
        )
        .child(spacer())
        .child(row().gap(6.0).children(m.tags.iter().map(|t| {
            label(t, sans(11.0, 400.0, 14.0), rgb(c::TAG_TEXT))
                .px(8.0)
                .py(2.0)
                .radius(9.0)
                .fill(rgb(c::TAG_BG))
        })));

    let card_bg = if m.mine { c::MINE_CARD } else { c::CARD };
    let card_border = if m.mine { c::MINE_BORDER } else { c::CARD_BORDER };
    let mut card = col()
        .gap(10.0)
        .px(16.0)
        .py(14.0)
        .fill(rgb(card_bg))
        .border(1.0, rgb(card_border))
        .radius(10.0)
        .border_layout()
        .child(head)
        .children(m.blocks.iter().map(|b| block(b, pictures)));

    if !m.mine {
        let act = |s: &str| {
            label(s, sans(12.0, 400.0, 16.0), rgb(c::TAG_TEXT))
                .px(10.0)
                .py(3.0)
                .radius(6.0)
                .fill(rgb(c::ACT_BG))
        };
        card = card.child(row().gap(8.0).child(act("Copy")).child(act("Retry")).child(act("Cite")));
    }
    card
}

fn view(s: &State, _: &mut Cx<State>) -> El {
    let content = col().key("content").gap(12.0).px(24.0).py(16.0).children(
        s.messages
            .iter()
            .map(|m| memo(format!("m{}", m.id), m.version, || msg_view(m, &s.pictures))),
    );
    col()
        .w_full()
        .h_full()
        .fill(rgb(c::BG))
        .child(col().key("scroller").grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(content))
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let t_start = Instant::now();
    let all = parse_corpus();
    let initial_msgs = all[..INITIAL_MESSAGES].to_vec();
    let extra_msgs = all[INITIAL_MESSAGES..].to_vec();
    let corpus_parse_ms = ms(t_start.elapsed());

    let t_img = Instant::now();
    let cache = ImageCache::new(Arc::new(|| {}));
    let pictures: Vec<Image> = PICTURES.iter().map(|b| cache.load(&b[..])).collect();
    let image_decode_ms = ms(t_img.elapsed());

    let t_font = Instant::now();
    let text_sys = TextSystem::new(FONTS, FAMILIES);
    let font_load_ms = ms(t_font.elapsed());

    let state = State { messages: initial_msgs, pictures, _cache: Some(cache) };

    let t_rt = Instant::now();
    let mut rt = Runtime::with_text(state, view, (WIDTH, HEIGHT), Env::new(true, 1.0), text_sys);
    let runtime_setup_ms = ms(t_rt.elapsed());

    // Clear any setup phase counters, then run Frame 0 (initial build + layout + shape + paint)
    let _ = take_frame_phases();
    let t_init = Instant::now();
    let _ = rt.frame(0.0);
    let init_frame_ms = ms(t_init.elapsed());
    let p_init = take_frame_phases();
    // phases: [0]=View, [1]=Commit(incl), [2]=Layout(incl), [3]=Text, [4]=Paint, [5]=Apply
    let init_view_ms = p_init[0];
    let init_commit_excl_layout_ms = (p_init[1] - p_init[2]).max(0.0);
    let init_text_shape_ms = p_init[3];
    let init_layout_pure_ms = (p_init[2] - p_init[3]).max(0.0);
    let init_paint_ms = p_init[4];
    let init_shaped_count = rt.text.shaped;
    let init_rasterized_glyphs = rt.text.atlas.rasterized();
    let cold_total_ms = ms(t_start.elapsed());

    // Phase 2: 30 Scroll frames (480px per frame)
    let _ = take_frame_phases();
    let mut scroll_p = [0.0f64; 6];
    let mut scroll_total_ms = 0.0f64;
    let dt = 1.0 / 60.0;
    for sf in 0..SCROLL_FRAMES {
        let y = (sf + 1) as f32 * SCROLL_STEP;
        let t0 = Instant::now();
        rt.update_if(|_, tx| {
            tx.scroll_to_instant("scroller", 0.0, y);
            false
        });
        let _ = rt.frame(dt);
        scroll_total_ms += ms(t0.elapsed());
        let p = take_frame_phases();
        for i in 0..6 {
            scroll_p[i] += p[i];
        }
    }

    // Prepare Phase 3: scroll to bottom + add streaming message
    let mut stream_id = 9000usize;
    rt.update_if(|s, tx| {
        s.messages.push(make_stream_message(stream_id));
        tx.scroll_to_bottom_instant("scroller");
        true
    });
    let _ = rt.frame(dt);
    let _ = take_frame_phases();

    let mut next_extra = 0usize;
    let mut stream_p = [0.0f64; 6];
    let mut stream_total_ms = 0.0f64;
    for tf in 0..STREAM_FRAMES {
        let t0 = Instant::now();
        rt.update_if(|s, tx| {
            apply_stream_step(&mut s.messages, &extra_msgs, &mut next_extra, tf, &mut stream_id);
            tx.scroll_to_bottom_instant("scroller");
            true
        });
        let _ = rt.frame(dt);
        stream_total_ms += ms(t0.elapsed());
        let p = take_frame_phases();
        for i in 0..6 {
            stream_p[i] += p[i];
        }
    }

    let sf_n = SCROLL_FRAMES as f64;
    let st_n = STREAM_FRAMES as f64;
    println!("{{");
    println!("  \"framework\": \"Rune\",");
    println!("  \"messages\": {},", INITIAL_MESSAGES);
    println!("  \"initial\": {{");
    println!("    \"corpus_parse_ms\": {:.4},", corpus_parse_ms);
    println!("    \"font_load_ms\": {:.4},", font_load_ms);
    println!("    \"image_decode_ms\": {:.4},", image_decode_ms);
    println!("    \"runtime_setup_ms\": {:.4},", runtime_setup_ms);
    println!("    \"view_ms\": {:.4},", init_view_ms);
    println!("    \"tree_build_ms\": {:.4},", init_commit_excl_layout_ms);
    println!("    \"style_ms\": 0.0,");
    println!("    \"text_shape_ms\": {:.4},", init_text_shape_ms);
    println!("    \"text_shape_calls\": {},", init_shaped_count);
    println!("    \"layout_total_ms\": {:.4},", p_init[2]);
    println!("    \"layout_pure_ms\": {:.4},", init_layout_pure_ms);
    println!("    \"paint_prep_ms\": {:.4},", init_paint_ms);
    println!("    \"glyphs_rasterized\": {},", init_rasterized_glyphs);
    println!("    \"frame_total_ms\": {:.4},", init_frame_ms);
    println!("    \"cold_total_ms\": {:.4}", cold_total_ms);
    println!("  }},");
    println!("  \"scroll_per_frame\": {{");
    println!("    \"update_ms\": {:.4},", (scroll_p[1] - scroll_p[2]).max(0.0) / sf_n);
    println!("    \"style_ms\": 0.0,");
    println!("    \"text_shape_ms\": {:.4},", scroll_p[3] / sf_n);
    println!("    \"layout_pure_ms\": {:.4},", (scroll_p[2] - scroll_p[3]).max(0.0) / sf_n);
    println!("    \"paint_prep_ms\": {:.4},", scroll_p[4] / sf_n);
    println!("    \"frame_total_ms\": {:.4}", scroll_total_ms / sf_n);
    println!("  }},");
    println!("  \"stream_per_frame\": {{");
    println!("    \"update_ms\": {:.4},", (stream_p[1] - stream_p[2]).max(0.0) / st_n);
    println!("    \"style_ms\": 0.0,");
    println!("    \"text_shape_ms\": {:.4},", stream_p[3] / st_n);
    println!("    \"layout_pure_ms\": {:.4},", (stream_p[2] - stream_p[3]).max(0.0) / st_n);
    println!("    \"paint_prep_ms\": {:.4},", stream_p[4] / st_n);
    println!("    \"frame_total_ms\": {:.4}", stream_total_ms / st_n);
    println!("  }}");
    println!("}}");
}
