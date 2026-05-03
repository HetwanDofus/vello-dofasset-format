//! Quick sanity check: open a SWF, decompress it, walk every tag,
//! and print exported sprites/shapes so we can confirm the Ruffle `swf`
//! crate actually parses Dofus 1.29 assets before we go further.

use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use swf::{decompress_swf, parse_swf, Tag};

fn main() -> Result<()> {
    let path: PathBuf = env::args()
        .nth(1)
        .context("usage: list-swf <path.swf>")?
        .into();

    let mut bytes = Vec::new();
    File::open(&path)
        .with_context(|| format!("open {}", path.display()))?
        .read_to_end(&mut bytes)?;

    // decompress_swf reads the 8-byte SWF header and returns a SwfBuf
    // wrapping the decompressed body bytes plus the parsed Header.
    let swf_buf = decompress_swf(bytes.as_slice())?;
    let swf = parse_swf(&swf_buf)?;

    println!("== {} ==", path.display());
    println!(
        "version={} stage={}x{} frame_rate={} num_frames={}",
        swf.header.version(),
        swf.header.stage_size().width().to_pixels(),
        swf.header.stage_size().height().to_pixels(),
        swf.header.frame_rate().to_f32(),
        swf.header.num_frames()
    );

    let mut sprite_count = 0usize;
    let mut shape_count = 0usize;
    let mut bitmap_count = 0usize;
    let mut other_count = 0usize;
    let mut exports: BTreeMap<String, u16> = BTreeMap::new();

    for tag in &swf.tags {
        match tag {
            Tag::DefineSprite(s) => {
                sprite_count += 1;
                if sprite_count <= 3 {
                    println!(
                        "  DefineSprite id={} num_frames={} inner_tags={}",
                        s.id,
                        s.num_frames,
                        s.tags.len()
                    );
                }
            }
            Tag::DefineShape(_) => shape_count += 1,
            Tag::DefineBitsLossless(_) => bitmap_count += 1,
            Tag::ExportAssets(assets) => {
                for a in assets {
                    exports.insert(a.name.to_string_lossy(swf::UTF_8).to_owned(), a.id);
                }
            }
            _ => other_count += 1,
        }
    }

    println!("totals:");
    println!("  sprites:   {}", sprite_count);
    println!("  shapes:    {}", shape_count);
    println!("  bitmaps:   {}", bitmap_count);
    println!("  other:     {}", other_count);
    println!("  exports:   {}", exports.len());

    // Numeric exports are the tile IDs we'll look up at render time.
    let mut numeric: Vec<(u32, u16)> = exports
        .iter()
        .filter_map(|(name, id)| name.parse::<u32>().ok().map(|n| (n, *id)))
        .collect();
    numeric.sort_by_key(|(n, _)| *n);
    println!("  numeric exports: {}", numeric.len());
    if !numeric.is_empty() {
        let (lo, _) = numeric.first().copied().unwrap();
        let (hi, _) = numeric.last().copied().unwrap();
        println!("    range: {}..={}", lo, hi);
        println!("    sample: {:?}", &numeric[..numeric.len().min(8)]);
    }

    // Non-numeric exports (e.g. anim names like "walkF") matter for sprites.
    let non_numeric: Vec<&String> = exports
        .keys()
        .filter(|k| k.parse::<u32>().is_err())
        .collect();
    if !non_numeric.is_empty() {
        // Dump them all when explicitly requested via env var.
        if env::var("DUMP_NON_NUMERIC").is_ok() {
            for n in &non_numeric {
                println!("    export: {}", n);
            }
        } else {
            let preview: Vec<&str> = non_numeric.iter().take(20).map(|s| s.as_str()).collect();
            println!("  non-numeric exports (first 20): {:?}", preview);
        }
    }

    Ok(())
}
