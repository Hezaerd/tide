use rune::{Color, El, LineCap, Shape, canvas, color::srgb_decode};

pub const WAVE_TOP: &str = "M1.5 6.2c1.6-1.6 3.2-1.6 4.8 0s3.2 1.6 4.8 0 2.4-1.2 3.4-.6";
pub const WAVE_BOTTOM: &str = "M1.5 10.6c1.6-1.6 3.2-1.6 4.8 0s3.2 1.6 4.8 0 2.4-1.2 3.4-.6";

#[derive(Clone, Copy)]
pub struct Theme {
    pub card: Color,
    pub text: Color,
    pub sub: Color,
    pub faint: Color,
    pub line: Color,
    pub track: Color,
    pub volume: Color,
    pub knob: Color,
    pub knob_ring: Color,
    pub hover: Color,
    pub off: Color,
}

impl Theme {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                card: hex(0x1b1c20),
                text: hex(0xecebe8),
                sub: hex(0x8f8d88),
                faint: hex(0x5f5e5a),
                line: hex(0x2a2b30),
                track: hex(0x2a2b30),
                volume: hex(0x43454c),
                knob: hex(0xf1f0ed),
                knob_ring: Color::BLACK.alpha(0.5),
                hover: Color::WHITE.alpha(0.04),
                off: hex(0xff6b57),
            }
        } else {
            Self {
                card: hex(0xf6f3ee),
                text: hex(0x26231f),
                sub: hex(0x8a8278),
                faint: hex(0xb3aa9e),
                line: hex(0xe6e0d7),
                track: hex(0xe3ddd4),
                volume: hex(0xcfc6b8),
                knob: Color::WHITE,
                knob_ring: Color::BLACK.alpha(0.14),
                hover: hex(0x26231f).alpha(0.045),
                off: hex(0xc2412d),
            }
        }
    }
}

pub fn rgb([r, g, b]: [u8; 3]) -> Color {
    let [r, g, b] = [r, g, b].map(|c| srgb_decode(f32::from(c) / 255.0));
    Color { r, g, b, a: 1.0 }
}

pub fn hex(code: u32) -> Color {
    let [_, r, g, b] = code.to_be_bytes();
    rgb([r, g, b])
}

pub fn wave(color: Color) -> El {
    canvas(
        Shape::new()
            .view_box(16.0, 16.0)
            .stroke_svg(WAVE_TOP, 1.6, LineCap::Round, color)
            .stroke_svg(WAVE_BOTTOM, 1.6, LineCap::Round, color.alpha(0.55)),
    )
    .key("wave")
    .size(16.0, 16.0)
    .a11y_hidden()
}
