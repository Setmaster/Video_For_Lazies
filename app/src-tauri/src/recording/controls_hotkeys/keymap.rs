//! Normalize an assignment using the actual layout mapper, never DOM code as VK.
use super::{Chord, KeyCapture, Modifiers};
pub(super) fn normalize(
    event: KeyCapture,
    mut scan: impl FnMut(u16) -> Result<i16, String>,
) -> Result<Chord, String> {
    if event.meta || !event.ctrl || (!event.shift && !event.alt) {
        return Err("Use Ctrl with Shift or Alt, without the Windows key.".into());
    }
    let modifiers = match (event.shift, event.alt) {
        (true, true) => Modifiers::CtrlShiftAlt,
        (true, false) => Modifiers::CtrlShift,
        _ => Modifiers::CtrlAlt,
    };
    // A single F is an ordinary letter and must pass through the layout mapper.
    let function = event
        .key
        .strip_prefix('F')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let key = if function {
        event.key
    } else {
        let mut chars = event.key.chars();
        let c = chars.next().ok_or("Choose a shortcut key.")?;
        if chars.next().is_some() || u32::from(c) > 0xffff {
            return Err("Unsupported shortcut key.".into());
        }
        // Letter case can come from Caps Lock rather than Shift. Canonical letter
        // identity is layout-derived independently of case; modifiers remain the
        // actual event modifiers. Symbols/digits retain their required shift state.
        let c = c.to_ascii_lowercase();
        let value = scan(c as u16)?;
        if value == -1 {
            return Err("This key cannot be mapped in the current Windows keyboard layout.".into());
        }
        let required = (value as u16) >> 8;
        if required & !7 != 0
            || required & 1 != 0 && !event.shift
            || required & 2 != 0 && !event.ctrl
            || required & 4 != 0 && !event.alt
        {
            return Err("Shortcut modifiers do not match the current keyboard layout.".into());
        }
        let vk = value as u16 & 255;
        if !(0x30..=0x39).contains(&vk) && !(0x41..=0x5a).contains(&vk) {
            return Err("Choose a letter, digit or supported function key.".into());
        }
        (vk as u8 as char).to_string()
    };
    let chord = Chord { modifiers, key };
    chord.validate()?;
    Ok(chord)
}
