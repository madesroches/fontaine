//! QR decoding of one camera luma plane, kept apart from the camera so it runs under host tests.

/// The text of the first QR code found in a tightly packed 8-bit luma image, if any.
pub fn decode_luma(width: usize, height: usize, luma: &[u8]) -> Option<String> {
    if width == 0 || height == 0 || luma.len() < width * height {
        return None;
    }
    let mut image =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| luma[y * width + x]);
    image
        .detect_grids()
        .into_iter()
        .find_map(|grid| grid.decode().ok().map(|(_, text)| text))
}

#[cfg(test)]
mod tests {
    use fontaine_remote::{EndpointAddr, SecretKey, Ticket};
    use qrcode::{Color, EcLevel, QrCode};

    use super::*;

    /// The code as a camera might see it: dark modules on a light frame, `scale` pixels a module.
    fn render(payload: &str, scale: usize) -> (usize, usize, Vec<u8>) {
        let code = QrCode::with_error_correction_level(payload, EcLevel::L).unwrap();
        let n = code.width();
        let quiet = 4;
        let side = (n + 2 * quiet) * scale;
        let colors = code.to_colors();
        let mut luma = vec![255u8; side * side];
        for y in 0..side {
            for x in 0..side {
                let (mx, my) = (x / scale, y / scale);
                let inside = mx >= quiet && my >= quiet && mx < quiet + n && my < quiet + n;
                if inside && colors[(my - quiet) * n + (mx - quiet)] == Color::Dark {
                    luma[y * side + x] = 0;
                }
            }
        }
        (side, side, luma)
    }

    #[test]
    fn the_terminal_qr_payload_decodes_back_to_a_ticket() {
        let key = SecretKey::from_bytes(&[7; 32]);
        let ticket = Ticket::new(EndpointAddr::new(key.public()), [9; 32], 1_700_000_000);
        let (w, h, luma) = render(&ticket.qr_payload(), 4);
        let text = decode_luma(w, h, &luma).expect("the code decodes");
        assert_eq!(Ticket::parse(&text).unwrap(), ticket);
    }

    #[test]
    fn an_image_with_no_code_decodes_to_nothing() {
        assert_eq!(decode_luma(64, 64, &vec![200; 64 * 64]), None);
        assert_eq!(decode_luma(0, 0, &[]), None);
        assert_eq!(decode_luma(10, 10, &[0; 5]), None);
    }
}
