//! The note text's fonts. egui's stock font is a single weight with no
//! bold or italic, so notes are set in the Ubuntu variable fonts (the same
//! family as egui's stock Ubuntu Light), whose `wght` axis gives bold and
//! whose italic face gives real italics (ADR-0003). The rest of the UI keeps
//! egui's stock fonts.

use std::sync::Arc;

use egui::epaint::text::VariationCoords;
use egui::{FontData, FontDefinitions, FontFamily, FontId};

const BODY: &str = "note-body";
const ITALIC: &str = "note-italic";

/// Body text weight: Light, as egui's stock font.
const REGULAR: f32 = 300.0;
const BOLD: f32 = 700.0;

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert(BODY.into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Ubuntu-Variable.ttf"))));
    fonts.font_data.insert(
        ITALIC.into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Ubuntu-Italic-Variable.ttf"))),
    );
    // Emoji and symbols fall back to egui's own fonts.
    let fallbacks: Vec<String> = fonts.families[&FontFamily::Proportional].iter().skip(1).cloned().collect();
    for (family, face) in [(BODY, BODY), (ITALIC, ITALIC)] {
        let mut list = vec![face.to_string()];
        list.extend(fallbacks.iter().cloned());
        fonts.families.insert(FontFamily::Name(family.into()), list);
    }
    ctx.set_fonts(fonts);
}

/// The font for note text of `size`, italic or not.
pub fn note(size: f32, italic: bool) -> FontId {
    FontId::new(size, FontFamily::Name(if italic { ITALIC } else { BODY }.into()))
}

/// The variable-font weight for bold or regular note text.
pub fn weight(bold: bool) -> VariationCoords {
    VariationCoords::new([(b"wght", if bold { BOLD } else { REGULAR })])
}
