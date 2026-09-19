//! The screen fallback (2b design): Mutter's own screencast and remote-desktop interfaces on the
//! session bus — no portal and no dialog, because the engine runs as the session's owner (Phase 0
//! U5). The pure parts come first (grid math, what ImageMagick is asked to draw, the took-over
//! test); the bus and the frame grabber after, proven live in the owner's VM (2b probes 1 and 2).
use std::collections::HashMap;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

pub const COLS: u32 = 8;
pub const ROWS: u32 = 6;
pub const SUB: u32 = 4;
pub const CELLS: u32 = COLS * ROWS;
pub const SPOTS: u32 = SUB * SUB;

/// The failure a screen action reports when the person touched the mouse or keyboard since the
/// AI's last look or move; the engine ends the job Stopped on it, never retries.
pub const TOOK_OVER: &str = "you moved the mouse or typed, so I stopped using the screen";

/// A cell's rectangle on a `w`×`h` screen as (x, y, width, height); cells are numbered from 1,
/// row by row. The edges come from one division each, so the cells tile the screen exactly.
pub fn cell_rect(cell: u32, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    if cell == 0 || cell > CELLS { return None; }
    let (r, c) = ((cell - 1) / COLS, (cell - 1) % COLS);
    let (x0, y0) = (c * w / COLS, r * h / ROWS);
    Some((x0, y0, (c + 1) * w / COLS - x0, (r + 1) * h / ROWS - y0))
}

/// The centre of `spot` inside `cell`, in screen pixels.
pub fn click_point(cell: u32, spot: u32, w: u32, h: u32) -> Option<(f64, f64)> {
    let (x, y, cw, ch) = cell_rect(cell, w, h)?;
    if spot == 0 || spot > SPOTS { return None; }
    let (r, c) = (f64::from((spot - 1) / SUB), f64::from((spot - 1) % SUB));
    let sub = f64::from(SUB);
    Some((f64::from(x) + (c + 0.5) * f64::from(cw) / sub, f64::from(y) + (r + 0.5) * f64::from(ch) / sub))
}

/// ImageMagick's arguments for what the model sees: the whole screen scaled to 1280 wide under the
/// cell grid, or one cell cropped from the full frame and enlarged to 1024 wide under the spot grid.
/// Numbers sit in each square's top-left corner on a pale box, so they read on any background.
pub fn picture_args(input: &str, output: &str, w: u32, h: u32, cell: Option<u32>) -> Vec<String> {
    let rect = cell.and_then(|c| cell_rect(c, w, h));
    let (src_w, src_h, cols, rows, out_w) = match rect { Some((_, _, cw, ch)) => (cw, ch, SUB, SUB, 1024), None => (w, h, COLS, ROWS, 1280) };
    let out_h = (u64::from(src_h) * u64::from(out_w) / u64::from(src_w.max(1))) as u32;
    let mut a: Vec<String> = vec![input.into()];
    if let Some((x, y, cw, ch)) = rect { a.extend(["-crop".into(), format!("{cw}x{ch}+{x}+{y}"), "+repage".into()]); }
    a.extend(["-resize".into(), format!("{out_w}x{out_h}!")]);
    a.extend(["-fill", "none", "-stroke", "#ff2a2a", "-strokewidth", "2"].map(String::from));
    for i in 1..cols { let x = i * out_w / cols; a.extend(["-draw".into(), format!("line {x},0 {x},{out_h}")]); }
    for j in 1..rows { let y = j * out_h / rows; a.extend(["-draw".into(), format!("line 0,{y} {out_w},{y}")]); }
    a.extend(["-stroke", "none", "-fill", "#d01010", "-undercolor", "#ffffffd8", "-pointsize", "22"].map(String::from));
    for n in 1..=cols * rows {
        let (r, c) = ((n - 1) / cols, (n - 1) % cols);
        a.extend(["-annotate".into(), format!("+{}+{}", c * out_w / cols + 4, r * out_h / rows + 24), n.to_string()]);
    }
    a.push(output.into());
    a
}

/// Did the person touch the mouse or keyboard since the AI last looked or moved? GNOME's idle time
/// restarts on any input, the AI's own included (2b probe 1), so an idle time shorter than the time
/// since the AI's last move means somebody else moved since. The slack covers the bus round trip.
pub fn took_over(idle_ms: u64, since_ai: Duration) -> bool { idle_ms + 300 < since_ai.as_millis() as u64 }

/// A PNG's width and height, from its header.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[1..4] != b"PNG" { return None; }
    let be = |i: usize| u32::from_be_bytes([png[i], png[i + 1], png[i + 2], png[i + 3]]);
    Some((be(16), be(20)))
}

/// The X keysym for a character: Latin-1 as itself, the rest in the Unicode keysym range, a new
/// line as Return.
pub fn keysym(c: char) -> u32 {
    match c { '\n' => RETURN, ' '..='~' | '\u{a0}'..='\u{ff}' => c as u32, _ => 0x0100_0000 + c as u32 }
}
pub const RETURN: u32 = 0xff0d;

/// What the hand remembers between screen actions, for the life of the process.
#[derive(Debug, Default)]
pub struct Screen {
    /// The cell the latest look zoomed into: the only one a click may land in (2b: two looks per click).
    pub last_zoom: Option<u32>,
    /// When the AI last looked or injected input: the took-over test measures from here.
    pub last_ai: Option<Instant>,
    /// The screen's size in pixels, from the latest frame.
    pub size: Option<(u32, u32)>,
}

const RD: &str = "org.gnome.Mutter.RemoteDesktop";
const SC: &str = "org.gnome.Mutter.ScreenCast";
const BTN_LEFT: i32 = 0x110;

fn bus_err(what: &str) -> impl Fn(zbus::Error) -> String + '_ { move |e| format!("{what}: {e}") }

/// One remote-desktop session linked to a screencast of the primary monitor. Opened per action and
/// stopped on drop, so GNOME's screen-sharing indicator shows exactly while the AI has the screen.
/// ponytail: a session per action costs well under a second next to a model call; keep one per
/// job if that ever shows.
struct Session { conn: Connection, rd: OwnedObjectPath, stream: OwnedObjectPath, node: u32 }

impl Session {
    fn open() -> Result<Self, String> {
        let conn = Connection::session().map_err(bus_err("no session bus"))?;
        let rd: OwnedObjectPath = conn.call_method(Some(RD), "/org/gnome/Mutter/RemoteDesktop", Some(RD), "CreateSession", &())
            .and_then(|m| m.body().deserialize()).map_err(bus_err("remote desktop"))?;
        let id: OwnedValue = conn.call_method(Some(RD), rd.as_str(), Some("org.freedesktop.DBus.Properties"), "Get", &(format!("{RD}.Session"), "SessionId"))
            .and_then(|m| m.body().deserialize()).map_err(bus_err("remote desktop session id"))?;
        let id = String::try_from(id).map_err(|e| format!("remote desktop session id: {e}"))?;
        let opts: HashMap<&str, Value> = HashMap::from([("remote-desktop-session-id", Value::from(id.as_str()))]);
        let sc: OwnedObjectPath = conn.call_method(Some(SC), "/org/gnome/Mutter/ScreenCast", Some(SC), "CreateSession", &(opts,))
            .and_then(|m| m.body().deserialize()).map_err(bus_err("screencast"))?;
        // An empty connector is Mutter's primary monitor. Cursor mode 1 draws the pointer into the frame.
        let rec: HashMap<&str, Value> = HashMap::from([("cursor-mode", Value::from(1u32))]);
        let stream: OwnedObjectPath = conn.call_method(Some(SC), sc.as_str(), Some(format!("{SC}.Session").as_str()), "RecordMonitor", &("", rec))
            .and_then(|m| m.body().deserialize()).map_err(bus_err("screencast monitor"))?;
        // The node id arrives only as a signal, so the watch is up before Start. It lives on its own
        // thread for the timeout; a stream that never starts leaves that thread waiting until the
        // bus connection goes, which is the session's end.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (node_tx, node_rx) = std::sync::mpsc::channel();
        let (c2, s2) = (conn.clone(), stream.clone());
        std::thread::spawn(move || {
            let watch = zbus::blocking::Proxy::new(&c2, SC, s2, format!("{SC}.Stream"))
                .and_then(|p| p.receive_signal("PipeWireStreamAdded"));
            let mut signals = match watch { Ok(s) => { let _ = ready_tx.send(true); s } Err(_) => { let _ = ready_tx.send(false); return; } };
            if let Some(m) = signals.next() { let _ = node_tx.send(m.body().deserialize::<u32>().ok()); }
        });
        if !ready_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false) { return Err("could not watch the screen stream".into()); }
        conn.call_method(Some(RD), rd.as_str(), Some(format!("{RD}.Session").as_str()), "Start", &()).map_err(bus_err("start remote desktop"))?;
        let node = node_rx.recv_timeout(Duration::from_secs(10)).ok().flatten().ok_or("the screen stream never started")?;
        Ok(Self { conn, rd, stream, node })
    }

    fn input(&self, method: &str, body: &(impl serde::Serialize + zbus::zvariant::DynamicType)) -> Result<(), String> {
        self.conn.call_method(Some(RD), self.rd.as_str(), Some(format!("{RD}.Session").as_str()), method, body)
            .map(|_| ()).map_err(bus_err(method))
    }

    /// One frame of the monitor as PNG bytes, through the Phase 0 pipeline.
    fn frame(&self) -> Result<Vec<u8>, String> {
        let path = scratch("frame.png");
        let _ = std::fs::remove_file(&path);
        let out = std::process::Command::new("gst-launch-1.0")
            .args(["-e", "-q", "pipewiresrc", &format!("path={}", self.node), "num-buffers=3", "!", "videoconvert", "!", "pngenc", "snapshot=true", "!", "filesink", &format!("location={path}")])
            .output().map_err(|e| format!("cannot run gst-launch-1.0 ({e}); the installer puts it in"))?;
        std::fs::read(&path).ok().filter(|b| b.len() > 1000)
            .ok_or_else(|| format!("no frame came from the screen: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }

    fn click(&self, x: f64, y: f64, double: bool) -> Result<(), String> {
        self.input("NotifyPointerMotionAbsolute", &(self.stream.as_str(), x, y))?;
        std::thread::sleep(Duration::from_millis(60));
        let clicks = if double { 2 } else { 1 };
        for _ in 0..clicks {
            self.input("NotifyPointerButton", &(BTN_LEFT, true))?;
            self.input("NotifyPointerButton", &(BTN_LEFT, false))?;
            std::thread::sleep(Duration::from_millis(60));
        }
        Ok(())
    }

    fn type_text(&self, text: &str, enter: bool) -> Result<(), String> {
        for k in text.chars().map(keysym).chain(enter.then_some(RETURN)) {
            self.input("NotifyKeyboardKeysym", &(k, true))?;
            self.input("NotifyKeyboardKeysym", &(k, false))?;
        }
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Let the last injected event land before the virtual devices go.
        std::thread::sleep(Duration::from_millis(150));
        let _ = self.input("Stop", &());
    }
}

/// A file under the user's runtime folder, private to them and gone at logout.
fn scratch(name: &str) -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    format!("{dir}/ai-os-screen-{name}")
}

fn idle_ms() -> Result<u64, String> {
    let conn = Connection::session().map_err(bus_err("no session bus"))?;
    conn.call_method(Some("org.gnome.Mutter.IdleMonitor"), "/org/gnome/Mutter/IdleMonitor/Core", Some("org.gnome.Mutter.IdleMonitor"), "GetIdletime", &())
        .and_then(|m| m.body().deserialize()).map_err(bus_err("idle monitor"))
}

/// The picture for the model, drawn by ImageMagick (`magick`, or `convert` where only v6 is).
fn picture(frame: &[u8], w: u32, h: u32, cell: Option<u32>) -> Result<Vec<u8>, String> {
    let (inp, out) = (scratch("in.png"), scratch("out.png"));
    std::fs::write(&inp, frame).map_err(|e| format!("cannot keep the frame: {e}"))?;
    let args = picture_args(&inp, &out, w, h, cell);
    let ran = std::process::Command::new("magick").args(&args).output()
        .or_else(|_| std::process::Command::new("convert").args(&args).output())
        .map_err(|e| format!("cannot run ImageMagick ({e}); the installer puts it in"))?;
    if !ran.status.success() { return Err(format!("ImageMagick failed: {}", String::from_utf8_lossy(&ran.stderr).trim())); }
    std::fs::read(&out).map_err(|e| format!("no picture came out: {e}"))
}

/// Refuses when the person has moved since the AI's last look or move (see `took_over`).
fn check_not_taken(st: &Screen) -> Result<(), String> {
    let Some(at) = st.last_ai else { return Err("look at the screen first".into()) };
    if took_over(idle_ms()?, at.elapsed()) { return Err(TOOK_OVER.into()); }
    Ok(())
}

/// Runs one screen action. `Ok` carries the detail line and, for a look, the picture.
pub fn run(st: &mut Screen, action: &crate::action::Action) -> Result<(String, Option<Vec<u8>>), String> {
    use crate::action::Action;
    match action {
        Action::ScreenLook { cell } => {
            if let Some(c) = cell {
                if cell_rect(*c, 1, 1).is_none() { return Err(format!("there is no square {c}: squares go from 1 to {CELLS}")); }
            }
            let s = Session::open()?;
            let frame = s.frame()?;
            let (w, h) = png_size(&frame).ok_or("the frame was not a picture")?;
            let pic = picture(&frame, w, h, *cell)?;
            st.size = Some((w, h));
            st.last_zoom = *cell;
            st.last_ai = Some(Instant::now());
            let line = match cell {
                None => format!("the screen ({w}×{h}) is in the picture under a grid of squares 1–{CELLS}; look at one square to aim inside it"),
                Some(c) => format!("square {c} enlarged, under a grid of spots 1–{SPOTS}; click one spot in square {c}, or look again"),
            };
            Ok((line, Some(pic)))
        }
        Action::ScreenClick { cell, spot, name, double } => {
            if st.last_zoom != Some(*cell) { return Err(format!("look at square {cell} first: a click lands only in the square the latest look enlarged")); }
            let (w, h) = st.size.ok_or("look at the screen first")?;
            let (x, y) = click_point(*cell, *spot, w, h).ok_or_else(|| format!("there is no spot {spot}: spots go from 1 to {SPOTS}"))?;
            check_not_taken(st)?;
            let s = Session::open()?;
            s.click(x, y, *double)?;
            st.last_ai = Some(Instant::now());
            // The screen has changed under the grid: the next click needs a fresh look.
            st.last_zoom = None;
            Ok((format!("clicked {name} at spot {spot} of square {cell}; look again to see what it did"), None))
        }
        Action::ScreenType { text, enter } => {
            check_not_taken(st)?;
            let s = Session::open()?;
            s.type_text(text, *enter)?;
            st.last_ai = Some(Instant::now());
            Ok((format!("typed {} characters{}; look again to see where they went", text.chars().count(), if *enter { " and pressed Enter" } else { "" }), None))
        }
        other => Err(format!("not a screen action: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_tile_the_screen_row_by_row() {
        assert_eq!(cell_rect(1, 1280, 800), Some((0, 0, 160, 133)));
        assert_eq!(cell_rect(8, 1280, 800), Some((1120, 0, 160, 133)));
        assert_eq!(cell_rect(9, 1280, 800), Some((0, 133, 160, 133)));
        assert_eq!(cell_rect(48, 1280, 800), Some((1120, 666, 160, 134)), "the last row takes the remainder");
        assert_eq!((cell_rect(0, 1280, 800), cell_rect(49, 1280, 800)), (None, None));
        // Every pixel column and row is in exactly one cell, on an odd-sized screen too.
        let (w, h) = (1366, 768);
        let total: u32 = (1..=CELLS).map(|c| { let (_, _, cw, ch) = cell_rect(c, w, h).unwrap(); cw * ch }).sum();
        assert_eq!(total, w * h);
    }

    #[test]
    fn a_spot_is_the_centre_of_its_sixteenth() {
        assert_eq!(click_point(1, 1, 1280, 800), Some((20.0, 133.0 / 8.0)));
        assert_eq!(click_point(1, 16, 1280, 800), Some((140.0, 133.0 * 7.0 / 8.0)));
        let (x, y) = click_point(48, 16, 1280, 800).unwrap();
        assert!(x < 1280.0 && y < 800.0);
        assert_eq!((click_point(1, 0, 1280, 800), click_point(1, 17, 1280, 800)), (None, None));
    }

    #[test]
    fn the_picture_is_the_whole_screen_or_one_cell_enlarged() {
        let whole = picture_args("in.png", "out.png", 1280, 800, None);
        assert_eq!(whole.first().map(String::as_str), Some("in.png"));
        assert_eq!(whole.last().map(String::as_str), Some("out.png"));
        assert!(!whole.contains(&"-crop".to_string()));
        assert!(whole.contains(&"1280x800!".to_string()));
        assert_eq!(whole.iter().filter(|a| *a == "-annotate").count(), CELLS as usize);

        let zoom = picture_args("in.png", "out.png", 1280, 800, Some(9));
        let i = zoom.iter().position(|a| a == "-crop").unwrap();
        assert_eq!(zoom[i + 1], "160x133+0+133");
        assert!(zoom.contains(&"1024x851!".to_string()));
        assert_eq!(zoom.iter().filter(|a| *a == "-annotate").count(), SPOTS as usize);
    }

    #[test]
    fn the_person_took_over_only_if_they_moved_after_the_ai() {
        let since = Duration::from_secs(10);
        assert!(!took_over(10_000, since), "idle since the AI's own move: nobody else touched it");
        assert!(!took_over(9_800, since), "within the bus slack");
        assert!(took_over(2_000, since), "input 2 s ago, the AI's was 10 s ago");
    }

    #[test]
    fn png_sizes_come_from_the_header() {
        let mut h = vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        h.extend(1280u32.to_be_bytes()); h.extend(800u32.to_be_bytes());
        assert_eq!(png_size(&h), Some((1280, 800)));
        assert_eq!(png_size(b"not a png at all, not at all"), None);
    }

    #[test]
    fn keysyms_for_text() {
        assert_eq!(keysym('a'), 0x61);
        assert_eq!(keysym('A'), 0x41);
        assert_eq!(keysym('\n'), RETURN);
        assert_eq!(keysym('é'), 0xe9);
        assert_eq!(keysym('Ω'), 0x0100_03a9);
    }

    #[test]
    fn a_click_needs_a_fresh_look_at_its_own_square() {
        let mut st = Screen { last_zoom: Some(3), last_ai: Some(Instant::now()), size: Some((1280, 800)) };
        let click = |cell| crate::action::Action::ScreenClick { cell, spot: 1, name: "OK".into(), double: false };
        let e = run(&mut st, &click(4)).unwrap_err();
        assert!(e.contains("look at square 4 first"), "{e}");
        st.last_zoom = None;
        assert!(run(&mut st, &click(3)).unwrap_err().contains("look at square 3 first"));
        let mut fresh = Screen::default();
        assert!(run(&mut fresh, &crate::action::Action::ScreenType { text: "x".into(), enter: false }).unwrap_err().contains("look at the screen first"));
        assert!(run(&mut fresh, &crate::action::Action::ScreenLook { cell: Some(49) }).unwrap_err().contains("no square 49"));
    }
}
