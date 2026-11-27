#!/usr/bin/env python3
"""Generate a deterministic, varied simulated chat session for chat-bench.

Writes:
  data/chat.txt   - line-oriented format for Silk and Rust harnesses
  data/chat.json  - identical content as JSON for Chromium
"""
import json
import os
import random

WORDS = (
    "the of and to in is that for it as with was on be by at this have from or one had not but what all were "
    "when we there can an your which their said if do will each about how up out them then she many some so "
    "these would other into has more her two like him see time could no make than first been its who now people "
    "my made over did down only way find use may water long little very after words called just where most know "
    "get through back much before go good new write our used me man too any day same right look think also around "
    "another came come work three word must because does part even place well such here take why things help put "
    "years different away again off went old number great tell men say small every found still between name should "
    "home big give air line set own under read last never us left end along while might next sound below saw "
    "renderer layout cascade selector gradient texture buffer shader pipeline frame budget latency throughput "
    "allocation footprint benchmark transcript paragraph compositor glyph shaping harfbuzz atlas rasterizer "
    "surface swapchain command encoder viewport scroller table column grid flexbox inline kerning ligature"
).split()

CODE_IDENTS = [
    "layout_node", "shape_run", "glyph_atlas", "paint_list", "frame_budget",
    "scroll_offset", "text_system", "scene_quad", "cache_entry", "line_breaker",
    "font_face", "raster_tile", "dirty_flags", "style_rule", "viewport_rect",
]

METRICS = [
    "Style recalc", "Text shaping", "Flex layout", "Grid tracks", "Table cells",
    "Inline wrap", "Paint commands", "Glyph atlas", "Image decode", "Compositor",
    "Memory RSS", "Scene finish", "Scroll replay", "Hit testing",
]

TAGS = ["analysis", "perf", "layout", "shaping", "gpu", "trace", "diff", "table", "media"]


def phrase(rng, lo, hi):
    return " ".join(rng.choice(WORDS) for _ in range(rng.randint(lo, hi)))


def sentence(rng):
    s = phrase(rng, 8, 20)
    return s[0].upper() + s[1:] + ("?" if rng.random() < 0.14 else ".")


def rich_spans(rng, sentences_lo, sentences_hi):
    """Build a list of (kind, text) spans: N=normal, B=bold, I=italic, C=code, L=link."""
    n_sent = rng.randint(sentences_lo, sentences_hi)
    spans = []
    for idx in range(n_sent):
        prefix = " " if idx > 0 else ""
        # Mix plain words with occasional inline marks
        r = rng.random()
        if r < 0.35:
            a = phrase(rng, 3, 7)
            a = a[0].upper() + a[1:]
            b = phrase(rng, 1, 3)
            c = phrase(rng, 3, 7)
            spans.append(("N", f"{prefix}{a} "))
            spans.append(("B", b))
            spans.append(("N", f" {c}."))
        elif r < 0.60:
            a = phrase(rng, 3, 6)
            a = a[0].upper() + a[1:]
            fn = rng.choice(CODE_IDENTS) + "()"
            c = phrase(rng, 3, 7)
            spans.append(("N", f"{prefix}{a} "))
            spans.append(("C", fn))
            spans.append(("N", f" {c}."))
        elif r < 0.80:
            a = phrase(rng, 3, 6)
            a = a[0].upper() + a[1:]
            it = phrase(rng, 2, 4)
            c = phrase(rng, 2, 5)
            spans.append(("N", f"{prefix}{a} "))
            spans.append(("I", it))
            spans.append(("N", f" {c}."))
        elif r < 0.92:
            a = phrase(rng, 3, 6)
            a = a[0].upper() + a[1:]
            lk = "docs/" + rng.choice(CODE_IDENTS)
            c = phrase(rng, 2, 5)
            spans.append(("N", f"{prefix}{a} "))
            spans.append(("L", lk))
            spans.append(("N", f" {c}."))
        else:
            spans.append(("N", prefix + sentence(rng)))
    # Coalesce adjacent N spans
    merged = []
    for k, t in spans:
        if merged and merged[-1][0] == k:
            merged[-1] = (k, merged[-1][1] + t)
        else:
            merged.append((k, t))
    return merged


def make_message(index):
    rng = random.Random(index * 7919 + 42)
    mine = (index % 2 == 0)
    hh = 9 + ((index // 12) % 12)
    mm = (index * 5) % 60
    ts = f"{hh:02d}:{mm:02d}"
    if mine:
        return {
            "id": index,
            "mine": True,
            "author": "You",
            "initials": "Y",
            "time": ts,
            "tags": ["prompt"],
            "blocks": [
                {"kind": "p", "spans": rich_spans(rng, 1, 3)},
            ],
        }

    turn = (index - 1) // 2
    tags = [TAGS[turn % len(TAGS)], TAGS[(turn * 3 + 1) % len(TAGS)]]
    blocks = [{"kind": "p", "spans": rich_spans(rng, 2, 4)}]

    # Tables on ~1/3 of assistant replies
    if turn % 3 == 0:
        head = ["Stage", "Before", "After", "Delta"]
        rows = []
        n_rows = 4 + (turn % 3)
        for r in range(n_rows):
            before = rng.randint(12, 480)
            after = rng.randint(8, 360)
            pct = (after - before) / before * 100.0
            rows.append([
                METRICS[(turn + r) % len(METRICS)],
                f"{before} us",
                f"{after} us",
                f"{pct:+.1f}%",
            ])
        blocks.append({"kind": "h", "text": phrase(rng, 3, 6).capitalize()})
        blocks.append({"kind": "table", "head": head, "rows": rows})

    # Images on ~1/4 of assistant replies
    if turn % 4 == 1:
        pic = turn % 4
        blocks.append({
            "kind": "img",
            "picture": pic,
            "width": 360,
            "height": 202,
            "caption": f"Figure {turn}: " + sentence(rng),
        })

    # Bullet lists on ~1/3 of assistant replies
    if turn % 3 == 1:
        items = [rich_spans(rng, 1, 2) for _ in range(rng.randint(3, 5))]
        blocks.append({"kind": "ul", "items": items})

    # Code blocks on ~1/5 of assistant replies
    if turn % 5 == 2:
        n_lines = 6 + (turn % 4)
        lines = []
        for l in range(n_lines):
            indent = "  " * (l % 3)
            kw = "const" if l % 3 == 0 else "let"
            v = rng.choice(CODE_IDENTS)
            fn = rng.choice(CODE_IDENTS)
            arg = rng.choice(WORDS)
            lines.append(f"{indent}{kw} {v}_{l} = {fn}({arg}, {rng.randint(1, 255)});")
        blocks.append({"kind": "pre", "lines": lines})

    # Blockquotes on ~1/4 of assistant replies
    if turn % 4 == 2:
        blocks.append({"kind": "q", "spans": rich_spans(rng, 1, 2)})

    # Closing paragraph
    blocks.append({"kind": "p", "spans": rich_spans(rng, 2, 3)})

    return {
        "id": index,
        "mine": False,
        "author": "Assistant",
        "initials": "A",
        "time": ts,
        "tags": tags,
        "blocks": blocks,
    }


def encode_spans(spans):
    return "\t".join(f"{k}:{t}" for k, t in spans)


def write_txt(messages, path):
    with open(path, "w", encoding="utf-8") as f:
        for m in messages:
            f.write(f"M|{m['id']}|{1 if m['mine'] else 0}|{m['author']}|{m['initials']}|{m['time']}|{','.join(m['tags'])}\n")
            for b in m["blocks"]:
                k = b["kind"]
                if k == "p":
                    f.write(f"P|{encode_spans(b['spans'])}\n")
                elif k == "q":
                    f.write(f"Q|{encode_spans(b['spans'])}\n")
                elif k == "h":
                    f.write(f"H|{b['text']}\n")
                elif k == "ul":
                    for item in b["items"]:
                        f.write(f"L|{encode_spans(item)}\n")
                elif k == "pre":
                    f.write("C|" + "\t".join(b["lines"]) + "\n")
                elif k == "table":
                    f.write("TH|" + "\t".join(b["head"]) + "\n")
                    for row in b["rows"]:
                        f.write("TR|" + "\t".join(row) + "\n")
                elif k == "img":
                    f.write(f"I|{b['picture']}|{b['width']}|{b['height']}|{b['caption']}\n")


def main():
    count = 500
    # Generate 500 initial messages + 20 extra messages for the live streaming phase
    messages = [make_message(i) for i in range(count + 20)]
    out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")
    os.makedirs(out_dir, exist_ok=True)
    write_txt(messages, os.path.join(out_dir, "chat.txt"))
    with open(os.path.join(out_dir, "chat.json"), "w", encoding="utf-8") as f:
        json.dump({"initialCount": count, "messages": messages}, f)

    # Print stats for the initial 500 messages
    init = messages[:count]
    counts = {"p": 0, "h": 0, "table": 0, "img": 0, "ul": 0, "pre": 0, "q": 0}
    words = 0
    for m in init:
        for b in m["blocks"]:
            counts[b["kind"]] += 1
            if "spans" in b:
                words += sum(len(t.split()) for _, t in b["spans"])
            elif b["kind"] == "ul":
                words += sum(len(t.split()) for item in b["items"] for _, t in item)
            elif b["kind"] == "table":
                words += sum(len(c.split()) for c in b["head"])
                words += sum(len(c.split()) for row in b["rows"] for c in row)
            elif b["kind"] == "pre":
                words += sum(len(l.split()) for l in b["lines"])
            elif b["kind"] == "img":
                words += len(b["caption"].split())
            elif b["kind"] == "h":
                words += len(b["text"].split())
    print(f"Generated {len(messages)} total messages ({count} initial + 20 stream reserve)")
    print(f"Initial 500 breakdown: {counts}, total words: {words}")


if __name__ == "__main__":
    main()
