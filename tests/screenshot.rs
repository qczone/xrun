#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires Xvfb with a 1024x768 24-bit X11 display"]
async fn x11_primary_display_png() {
    let capture = xrun::screenshot::capture().await.unwrap();
    assert_eq!((capture.width, capture.height), (1024, 768));
    assert!(capture.at.ends_with('Z'));
    let decoder = png::Decoder::new(std::io::Cursor::new(capture.bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(&pixels[..3], &[0, 0, 0]);
}
