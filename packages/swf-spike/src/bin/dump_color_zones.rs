use anyhow::Result;
use std::path::PathBuf;
use swf_spike::swf_doc::{Symbol, SwfDoc};

fn main() -> Result<()> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap());
    let doc = SwfDoc::load(&path)?;
    let mut zones: Vec<(u16, u8)> = doc
        .sprite_color_zones
        .iter()
        .map(|(k, v)| (*k, *v))
        .collect();
    zones.sort();
    println!("{} sprites tagged", zones.len());
    let mut by_zone: std::collections::BTreeMap<u8, Vec<u16>> = Default::default();
    for (id, z) in &zones {
        by_zone.entry(*z).or_default().push(*id);
    }
    for (z, ids) in &by_zone {
        println!("zone {}: {} sprites — {:?}", z, ids.len(), &ids[..ids.len().min(15)]);
    }
    // also report which exports map to a zoned sprite
    println!("\nExport name → zone:");
    let mut exp_zones: Vec<(String, u16, u8)> = doc
        .by_name
        .iter()
        .filter_map(|(name, id)| doc.sprite_color_zones.get(id).map(|z| (name.clone(), *id, *z)))
        .collect();
    exp_zones.sort();
    for (n, id, z) in exp_zones.iter().take(20) {
        println!("  {} (id={}) → zone {}", n, id, z);
        let _: Option<&Symbol> = doc.lookup_id(*id);
    }
    Ok(())
}
