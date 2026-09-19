//! Keeps the machine awake while the AI works. The owner's finding (2026-09-19): Ubuntu went to
//! sleep after a while and the job in hand died with it. GNOME's session manager holds an
//! inhibitor for as long as the bus connection that asked for it lives, so the guard keeps that
//! connection and lets go on drop. Only while working: the person's own power settings stand the
//! rest of the time, and nothing is switched off for good.
use zbus::blocking::Connection;

const SM: &str = "org.gnome.SessionManager";
/// GNOME's inhibit flags: 4 suspending, 8 the session going idle (the screen blanking and locking,
/// which would also blind the screen hand).
const SUSPEND_AND_IDLE: u32 = 4 | 8;

pub struct Awake { conn: Connection, cookie: u32 }

/// `None` where there is no GNOME session to ask (tests, CI, a bare service): nothing sleeps there
/// that this could stop, and a job must never fail for want of it.
pub fn hold(reason: &str) -> Option<Awake> {
    let conn = Connection::session().ok()?;
    let cookie: u32 = conn.call_method(Some(SM), "/org/gnome/SessionManager", Some(SM), "Inhibit", &("org.aios.Rail", 0u32, reason, SUSPEND_AND_IDLE))
        .ok()?.body().deserialize().ok()?;
    Some(Awake { conn, cookie })
}

impl Drop for Awake {
    fn drop(&mut self) {
        let _ = self.conn.call_method(Some(SM), "/org/gnome/SessionManager", Some(SM), "Uninhibit", &(self.cookie,));
    }
}
