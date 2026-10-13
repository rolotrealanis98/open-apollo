//! View > Show/Hide Channels (⌘I): `Rack_ShowHideChannels::perform` (0x180918150) toggles
//! window page 8. The page lists every input channel with a show toggle; turning one off sets
//! its `ChannelHidden`, and Console's mixer then leaves the strip out (the strip-label menu's
//! Hide does the same from the other side).
//!
//! No capture of the page exists yet, so it is drawn with Console's dialog chrome (the
//! settings `window_frame`) and Switch-Small toggles; its geometry is not 1:1-verified.

use crate::draw::Align;
use houston::Rect;
use crate::input::{Control, ControlId};
use crate::model::MixerModel;
use crate::surfaces::settings::window_frame;
use crate::ui::Ui;

/// Window size in points (title bar included).
pub const SIZE: (f32, f32) = (300.0, 420.0);
const ROW_H: f32 = 24.0;

fn cid(strip: &str, control: Control) -> ControlId {
    ControlId { strip: strip.into(), control }
}

/// Draws the window centred in `bounds`.
pub fn draw(ui: &mut Ui, model: &dyn MixerModel, bounds: Rect) {
    let (w, h) = SIZE;
    let r = Rect::new((bounds.x + (bounds.w - w) / 2.0).round(), (bounds.y + (bounds.h - h) / 2.0).round(), w, h);
    let body = window_frame(ui, r, "SHOW/HIDE CHANNELS", cid("channels", Control::ChannelsClose));
    let label = ui.style("fonts", "Channel-Strip-Section-Header");
    let mut y = body.y + 8.0;
    for ch in model.channel_list() {
        if y + ROW_H > body.max_y() {
            break;
        }
        let row = Rect::new(body.x + 12.0, y, body.w - 24.0, ROW_H);
        ui.text(&ch.name, label.clone(), Rect::new(row.x, row.y, row.w - 50.0, row.h), Align::Left);
        let toggle = Rect::new(row.max_x() - 44.0, row.y + 2.5, 44.0, 19.0);
        ui.toggle("Switch-Small", Some("SHOW"), !ch.hidden, true, toggle, Some(cid(&ch.id, Control::ChannelVisible)), None);
        y += ROW_H;
    }
}
