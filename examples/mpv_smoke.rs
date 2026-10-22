//! Headless libmpv smoke test: `cargo run --example mpv_smoke -- <media.mkv>`
//! Mirrors create_mpv() in src/stremio_app/stremio_player/player.rs, minus the window
//! (`wid`) and GPU options, with vo=null / ao=null. Which libmpv-2.dll is used is decided
//! by PATH (prepend the DLL directory to test another build).
use libmpv2::{events::Event, Mpv};
use std::time::{Duration, Instant};

struct Track {
    id: i64,
    kind: String,
    lang: String,
    title: String,
}

fn tracks(mpv: &Mpv) -> Vec<Track> {
    let n = mpv.get_property::<i64>("track-list/count").unwrap_or(0);
    (0..n)
        .map(|i| Track {
            id: mpv.get_property::<i64>(&format!("track-list/{i}/id")).unwrap_or(-1),
            kind: mpv.get_property::<String>(&format!("track-list/{i}/type")).unwrap_or_default(),
            lang: mpv.get_property::<String>(&format!("track-list/{i}/lang")).unwrap_or_default(),
            title: mpv.get_property::<String>(&format!("track-list/{i}/title")).unwrap_or_default(),
        })
        .collect()
}

fn pos(mpv: &Mpv) -> f64 {
    mpv.get_property::<f64>("time-pos").unwrap_or(-1.0)
}

/// Same shape as laststream-app/src/player/shell.ts loadfileArgs().
fn loadfile_args(url: &str, start: f64, new_syntax: bool) -> Vec<String> {
    if !(start > 0.0) {
        return vec![url.into()];
    }
    let s = format!("start=+{start}");
    if new_syntax {
        vec![url.into(), "replace".into(), "-1".into(), s]
    } else {
        vec![url.into(), "replace".into(), s]
    }
}

fn version_at_least(v: &str, min: (u32, u32)) -> bool {
    let nums: Vec<u32> = v
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .take(2)
        .filter_map(|s| s.parse().ok())
        .collect();
    nums.len() == 2 && (nums[0], nums[1]) >= min
}

fn main() {
    let file = std::env::args().nth(1).expect("usage: mpv_smoke <media file>");
    let mpv = Mpv::with_initializer(|i| {
        i.set_property("audio-client-name", "Laststream")?;
        i.set_property("msg-level", "all=no")?;
        i.set_property("quiet", "yes")?;
        i.set_property("hwdec", "auto")?;
        i.set_property(
            "stream-lavf-o",
            "reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_on_http_error=%23%408,429,500,502,503,504,reconnect_delay_max=15",
        )?;
        i.set_property("vo", "null")?;
        i.set_property("ao", "null")?;
        i.set_property("force-window", "no")?;
        Ok(())
    })
    .expect("cannot build MPV");
    let version = mpv.get_property::<String>("mpv-version").unwrap_or_default();
    let new_syntax = version_at_least(&version, (0, 39));
    println!("libmpv: {version} (loadfile new syntax: {new_syntax})");
    let _ = mpv.disable_deprecated_events();

    let mut rows: Vec<(String, bool, String)> = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| rows.push((name.to_string(), ok, detail));

    // Resume syntax is exercised with a 2 s start; the shell only sends it when startAt > 0.
    let args = loadfile_args(&file, 2.0, new_syntax);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    mpv.command("loadfile", &argv).expect("loadfile failed");

    let t0 = Instant::now();
    let mut loaded = false;
    let mut load_s = 0.0;
    let mut first_pos = None;
    let mut adv: Option<(f64, f64)> = None; // (advance, wall)
    let mut ended = false;
    let mut end_reason = String::new();
    let (mut audio, mut selected_aid, mut fr_id) = (Vec::new(), -1i64, -1i64);
    let mut dur = 0.0;
    let mut sought: Option<f64> = None;
    let mut seek_requested = false;
    let mut selected = false;
    let deadline = Instant::now() + Duration::from_secs(60);

    while Instant::now() < deadline && !ended {
        match mpv.wait_event(0.1) {
            Some(Ok(Event::FileLoaded)) => { loaded = true; load_s = t0.elapsed().as_secs_f64(); }
            Some(Ok(Event::EndFile(r))) => {
                ended = true;
                end_reason = format!("{r:?}");
            }
            _ => {}
        }
        if !loaded {
            continue;
        }
        let p = pos(&mpv);
        if first_pos.is_none() && p >= 0.0 {
            first_pos = Some((p, Instant::now()));
            dur = mpv.get_property::<f64>("duration").unwrap_or(0.0);
            audio = tracks(&mpv).into_iter().filter(|t| t.kind == "audio").collect();
            // Same selection as the web app: by language fre/fra, then aid <- track id.
            if let Some(t) = audio.iter().find(|t| t.lang == "fre" || t.lang == "fra") {
                fr_id = t.id;
                mpv.set_property("aid", t.id).expect("set aid");
                selected = true;
            }
        }
        if let Some((p0, ts)) = first_pos {
            if adv.is_none() && p - p0 >= 3.0 {
                adv = Some((p - p0, ts.elapsed().as_secs_f64()));
                selected_aid = mpv.get_property::<i64>("aid").unwrap_or(-1);
                mpv.command("seek", &["15", "absolute"]).expect("seek");
                seek_requested = true;
            } else if seek_requested && sought.is_none() && p >= 14.5 {
                sought = Some(p);
            }
        }
    }

    let sp = first_pos.map(|x| x.0).unwrap_or(-1.0);
    check("file loaded", loaded, format!("after {load_s:.2}s"));
    check("resume start=+2 honoured", (1.5..4.0).contains(&sp), format!("first time-pos {sp:.2}s"));
    check("duration ~20s", (dur - 20.0).abs() < 1.0, format!("{dur:.2}s"));
    let (a, w) = adv.unwrap_or((0.0, 0.0));
    check("time-pos advances >=3s within 10s wall", adv.is_some() && w <= 10.0, format!("+{a:.2}s media in {w:.2}s wall"));
    let langs: Vec<String> = audio.iter().map(|t| format!("{}:{}({})", t.id, t.lang, t.title)).collect();
    let has = |l: &str| audio.iter().any(|t| t.lang == l);
    check("2 audio tracks jpn+fre", audio.len() == 2 && has("jpn") && has("fre"), langs.join(" "));
    check("aid -> fre track after select", selected && selected_aid == fr_id && fr_id > 0, format!("aid={selected_aid} fre_id={fr_id}"));
    check("seek to 15s", sought.is_some(), format!("time-pos {:.2}s", sought.unwrap_or(-1.0)));
    check("end-file fires", ended, format!("reason {end_reason}, {:.1}s total", t0.elapsed().as_secs_f64()));

    println!("\n{:<42} {:<5} {}", "CHECK", "RES", "VALUE");
    for (n, ok, d) in &rows {
        println!("{:<42} {:<5} {}", n, if *ok { "PASS" } else { "FAIL" }, d);
    }
    if rows.iter().any(|r| !r.1) {
        std::process::exit(1);
    }
}
