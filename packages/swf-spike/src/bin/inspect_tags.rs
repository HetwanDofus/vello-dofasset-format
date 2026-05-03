use std::fs::File;
use std::io::Read;
use swf::{decompress_swf, parse_swf, Tag};
use swf::avm1::read::Reader;

fn dump_action(prefix: &str, data: &[u8]) {
    println!("{} ({} bytes):", prefix, data.len());
    let mut r = Reader::new(data, 7);
    loop {
        match r.read_action() {
            Ok(swf::avm1::types::Action::End) => { println!("  End"); break }
            Ok(a) => println!("  {:?}", a),
            Err(_) => break,
        }
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("path");
    let mut bytes = Vec::new();
    File::open(&path).unwrap().read_to_end(&mut bytes).unwrap();
    let buf = decompress_swf(bytes.as_slice()).unwrap();
    let swf = parse_swf(&buf).unwrap();
    let mut frame = 1;
    println!("== ROOT TAGS ==");
    for tag in &swf.tags {
        match tag {
            Tag::DoAction(d) => dump_action(&format!("Root DoAction f{}", frame), d),
            Tag::DoInitAction { id, action_data } => dump_action(&format!("DoInitAction id={}", id), action_data),
            Tag::ShowFrame => { frame += 1; }
            _ => {}
        }
    }
    for tag in &swf.tags {
        if let Tag::DefineSprite(s) = tag {
            println!("\n== SPRITE {} ({} frames) ==", s.id, s.num_frames);
            let mut f = 1;
            for inner in &s.tags {
                match inner {
                    Tag::DoAction(d) => dump_action(&format!("  f{} DoAction", f), d),
                    Tag::PlaceObject(p) => {
                        if let Some(actions) = &p.clip_actions {
                            for ca in actions {
                                let evs: Vec<&str> = [
                                    (swf::ClipEventFlag::LOAD, "LOAD"),
                                    (swf::ClipEventFlag::ENTER_FRAME, "ENTER_FRAME"),
                                    (swf::ClipEventFlag::UNLOAD, "UNLOAD"),
                                    (swf::ClipEventFlag::INITIALIZE, "INITIALIZE"),
                                    (swf::ClipEventFlag::CONSTRUCT, "CONSTRUCT"),
                                ].iter().filter(|(f, _)| ca.events.contains(*f)).map(|(_, n)| *n).collect();
                                if !evs.is_empty() {
                                    dump_action(&format!("  f{} ClipAction depth={} events={:?}", f, p.depth, evs), &ca.action_data);
                                }
                            }
                        }
                    }
                    Tag::ShowFrame => { f += 1; }
                    _ => {}
                }
            }
        }
    }
}
