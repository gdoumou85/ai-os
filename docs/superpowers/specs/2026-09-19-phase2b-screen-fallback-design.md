# Phase 2b — the screen fallback — design

2026-09-19. Approved in conversation with the owner. The parent is `2026-09-15-ai-os-design.md` (§4.4
tier 3, decision 13). The sibling is `2026-09-17-phase2a-desktop-hand-design.md`.

## Why

The desktop hand (2a) works a window through its accessibility controls. Firefox pages are shallow
there, and Electron apps (VS Code, Discord) expose nothing. The owner wants the AI to reach "all of
them eventually". The screen is the last resort: look at the pixels, click, type.

## Decisions (the owner's)

1. **Aiming is grid, then zoom.** Small local models read a screenshot well but miss by tens of pixels
   when asked for coordinates. A look shows the screen under a numbered 8 × 6 grid. A look at one
   cell shows that cell enlarged under a 4 × 4 grid. A click names a cell and a spot. That's two
   looks per click, and the accuracy doesn't depend on the model's pointing skill.
2. **It works on the person's own screen, and stops if the person moves.** A notice shows while it
   happens. Any input from the person since the AI's last move stops the job, and the job says why.
3. **It's a last resort.** The prompt says to use a window's controls first (2a) and the screen only
   for what they can't reach.

## The hands (`executor::action::Action`)

- `screen_look { cell: Option<u32> }`. No cell: the whole first monitor, scaled to 1280 px wide,
  with the grid and its numbers drawn on it. Cells are numbered 1–48, row by row. A cell: that
  cell cropped from the full-resolution frame, enlarged to 1024 px wide, with a 4 × 4 grid numbered
  1–16. Result: `ok` with a one-line description and the image.
- `screen_click { cell: u32, spot: u32, name: String, #[serde(default)] double: bool }`. It clicks
  the centre of `spot` in `cell`. The worker refuses unless the latest `screen_look` was of that
  cell. The refusal says "look at cell N first". This is the same "ids come from the latest look"
  rule 2a has. `name` is what the model says it is clicking, and it goes to the risk rule.
- `screen_type { text: String, #[serde(default)] enter: bool }`. It types into whatever has the
  focus, and presses Enter after when asked.

Grid math is one pure function each way, from cell/spot to monitor pixels, and it's unit-tested.

## Safety

- **Decision 9 applies unchanged.** `rules::risky_press(name)` on a `screen_click` → Needs-your-OK.
  The OK card carries the zoomed cell image (the rail shows it like a thumbnail) and the words "as
  the AI reads the screen". The name is the model's claim and nothing verifies it, so the card says
  so.
- **No undo inside a window**, stated on the Done card as in 2a.
- **Stop on the person's input.** Before every `screen_click` and `screen_type`, the worker asks
  `org.gnome.Mutter.IdleMonitor` (`/org/gnome/Mutter/IdleMonitor/Core`, `GetIdletime`) how long the
  input has been idle. If it's less than the time since the worker's own last injected event, the
  person has touched the mouse or keyboard. The action fails with "you moved the mouse or typed, so
  I stopped", and the job ends Stopped. **This is unproven.** Whether Mutter counts the injected
  events as activity decides the exact comparison, and the first plan task is a probe in the
  owner's VM. If the idle monitor can't tell the two apart, the fallback is the rail's Stop
  button and the notice text changes to "press Stop to take over". The job is never left running
  on an unguarded screen without the person knowing.
- **The notice.** GNOME's own screen-sharing indicator shows while the session is open. The rail
  shows a status line, "AI is using the screen — move the mouse to stop", from a new
  `Event::Busy`-style line when the first screen action of a job runs.

## Under the hood

- **Session.** One Mutter `RemoteDesktop` session is linked to a `ScreenCast` session recording the
  first monitor (`RecordMonitor`, `cursor-mode` embedded). It's opened by the first screen action
  of a job and closed when the job ends. It goes over zbus, which the executor already uses for
  AT-SPI. There's no portal and no dialog, because the engine runs as the session owner (Phase 0 U5).
- **Frames.** `gst-launch-1.0 -q pipewiresrc path=<node> num-buffers=1 ! videoconvert ! pngenc !
  filesink`, a proven Phase 0 pipeline. It's the lazy path, with no PipeWire bindings.
- **Grid and zoom.** ImageMagick (`magick`, or `convert` when that's all there is) crops, scales,
  and draws the grid lines and numbers. `ponytail:` it shells out per look, which is fine at
  seconds per model call. Move to in-process drawing if looks become the bottleneck.
- **Input.** `NotifyPointerMotionAbsolute(stream, x, y)`, then `NotifyPointerButton(BTN_LEFT=0x110,
  true/false)` (twice for `double`). Typing uses `NotifyKeyboardKeysym` per character, and Enter is
  XK_Return.
- **Images to the model.** `Prompt` gains `image: Option<Vec<u8>>`. The engine attaches the
  latest screen image to the next model call only: one image, so the 8k budget holds. Ollama gets
  `messages[user].images = [base64]`. The OpenAI door (LM Studio) gets user `content` as
  `[{type:"text"}, {type:"image_url", image_url:{url:"data:image/png;base64,…"}}]`. A model
  without vision gets a clear error from its runner, and the job says so.
- **Installer.** `apt-get install gstreamer1.0-tools gstreamer1.0-pipewire imagemagick`.

## Tests

- Unit (CI): grid math both ways, including the edges and a non-16:9 monitor. Action serde. The
  `risky_press` path for `screen_click` (a Needs-your-OK carrying the cell image). The worker refuses
  a click on a cell that wasn't the latest zoom. The idle comparison as a pure function. `Prompt`
  images in both bodies (Ollama `images`, OpenAI content parts). A prompt with no image is
  byte-identical to today's.
- Probe (owner's VM, first): does injected input reset `GetIdletime`, and does real input?
- Live acceptance (owner's VM): click "Show Apps" on the GNOME dock; open Firefox at a page and
  click a named link, then check the page changed; a run where the person moves the mouse
  mid-job and the job ends Stopped with the reason.

## Not in this

Several monitors (the first only), dragging, scrolling (a later hand if the acceptance needs it),
browsers through their own automation (CDP/WebDriver), the AI's own invisible screen, and KDE.
