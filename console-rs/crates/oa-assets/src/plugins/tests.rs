//! Bit-exactness against Pillow 12.3.0 on synthetic images. The expected
//! hashes come from running the same operations in Pillow on the same
//! pseudo-random pixels; no UA data is involved.

use super::img::Rgba;
use super::resample::{Filter, resize, resize_nearest};

fn xs(seed: u32, n: usize) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 24) as u8
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Alpha {
    Opaque,
    Mixed,
}

fn synth(w: u32, h: u32, seed: u32, a: Alpha) -> Rgba {
    let mut px = xs(seed, w as usize * h as usize * 4);
    for p in px.as_chunks_mut::<4>().0 {
        p[3] = match a {
            Alpha::Opaque => 255,
            Alpha::Mixed if p[3] < 60 => 0,
            Alpha::Mixed if p[3] > 200 => 255,
            Alpha::Mixed => p[3],
        };
    }
    Rgba::from_raw(w, h, px).unwrap()
}

const PILLOW: &[(&str, &str)] = &[
    ("src", "5c5d473a204998535bfddd0d42bbc831988c610e2c7a6ba9d158dd1bf523da04"),
    ("lanczos_2x", "9e3ab1a8a05d3abe74e699e69b1af3d220bc2939b9ccf6ca5485d09e2d70067a"),
    ("lanczos_down", "939ad3f15bc6705d7048d5b51366dda5396c534a06eda6e09d0b390b7fd7b0f4"),
    ("lanczos_odd", "9c3de41ab419d9e444b13158d4473c346f651c8bec73e9717789d3e5c9df79db"),
    ("lanczos_vonly", "499ffd09533da00067349b7d67af2d8b50d97c131d3a03fe5b40cb1042ae4126"),
    ("bilinear", "4dd788c56b1615c59562b8d484e97d99d0405138322a8fd0a9c0f47c6af59f37"),
    ("bicubic", "3a4d89c45f3bcfddcf6020301be2af5bf661a4cddb6b32a97c061685f2c696ed"),
    ("box", "046636a88ff3ee9471044ee10ea7af45e83ff248f2bdb222d2ad3c6cede25f8a"),
    ("hamming", "b4ae3d268ba9e447412451790a85c1c38c5e0d4e6145eee6741b70cab735d808"),
    ("lanczos_box", "7bd02a08d9951723c16f44dbd94e94f58a37efe6341e89b6cd86ad623558871a"),
    ("nearest", "cdee5c9fca849fd329c063018ae4e02053538bde024e54fa3fb8e16ffcfc0748"),
    ("nearest_down", "aae72e1864515267b9e348010676884c38de46e6383a11c8a592d0944def73e7"),
    ("composite_clip", "332eeffebd7216abe4edc0c8d04d666e0a653bdc994883e52dffaf5e3ac42d5a"),
    ("paste_clip", "cebdb0e6c936dc0586a2817966d9e6f079ccdfd42a84bfdfe9b14dce53e14c00"),
    ("paste_mask", "794b6686748b8af978ab99b12f06ec70867a66183f6d7772665d4e4cbf4176d2"),
    ("crop_pad", "49e3fb0ac1d900a4adfda48b5ae5cfb4c516ab6db94e3d7b14e4a8504b06e6eb"),
];

fn want(name: &str) -> &'static str {
    PILLOW.iter().find(|(k, _)| *k == name).map(|(_, v)| *v).unwrap()
}

#[test]
fn resample_matches_pillow() {
    let src = synth(37, 23, 1, Alpha::Mixed);
    assert_eq!(src.hash(), want("src"));
    type Case = (&'static str, u32, u32, Filter, Option<[f32; 4]>);
    let cases: &[Case] = &[
        ("lanczos_2x", 74, 46, Filter::Lanczos, None),
        ("lanczos_down", 15, 9, Filter::Lanczos, None),
        ("lanczos_odd", 50, 23, Filter::Lanczos, None),
        ("lanczos_vonly", 37, 61, Filter::Lanczos, None),
        ("bilinear", 29, 40, Filter::Bilinear, None),
        ("bicubic", 60, 11, Filter::Bicubic, None),
        ("box", 12, 8, Filter::Box, None),
        ("hamming", 20, 30, Filter::Hamming, None),
        ("lanczos_box", 40, 40, Filter::Lanczos, Some([3.5, 2.25, 30.0, 20.0])),
    ];
    for &(name, w, h, f, b) in cases {
        assert_eq!(resize(&src, w, h, f, b).hash(), want(name), "{name}");
    }
    assert_eq!(resize_nearest(&src, 80, 13, None).hash(), want("nearest"));
    assert_eq!(resize_nearest(&src, 11, 7, None).hash(), want("nearest_down"));
}

#[test]
fn compositing_matches_pillow() {
    let dst = synth(30, 20, 3, Alpha::Mixed);
    let mut t = dst.clone();
    t.composite(&synth(16, 16, 9, Alpha::Mixed), 20, 10);
    assert_eq!(t.hash(), want("composite_clip"));
    let mut t = dst.clone();
    t.paste(&synth(16, 16, 7, Alpha::Opaque), -5, 12);
    assert_eq!(t.hash(), want("paste_clip"));
    let m = synth(16, 16, 11, Alpha::Mixed);
    let mut t = dst.clone();
    t.paste_masked(&m, &m.alpha(), 4, 4);
    assert_eq!(t.hash(), want("paste_mask"));
    assert_eq!(dst.crop(-3, -4, 38, 14).hash(), want("crop_pad"));
}

/// JPEGs that Pillow 12.3.0 (libjpeg-turbo) wrote from synthetic pixels, and
/// the RGBA hash Pillow decodes them to.
const JPEGS: &[(&str, &str, &str)] = &[
    ("rgb444", "614e8e85e2e38775c71c72ee9f5a7e5edf4e8bb3b7cd9839de1eee92491aeddb", concat!(
        "ffd8ffe000104a46494600010100000100010000ffdb0043000503040404030504040405050506070c08070707070f0b",
        "0b090c110f1212110f111113161c1713141a1511111821181a1d1d1f1f1f13172224221e241c1e1f1effdb0043010505",
        "050706070e08080e1e1411141e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e",
        "1e1e1e1e1e1e1e1e1e1e1e1e1e1effc0001108000d001303011100021101031101ffc4001f0000010501010101010100",
        "000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d010203000411051221",
        "31410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a",
        "434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a9293949596979899",
        "9aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1",
        "f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100",
        "020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f015",
        "6272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a63646566676869",
        "6a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4",
        "c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffda000c03010002110311003f00f0",
        "fd17c1a8b791a4d03ac81c72325c8c1ed8f5c0078ce79ebcf3d3aca692ba976eabbf75f3bbd3b76e2cb33b5565ed378f",
        "aa49597dfa792d34b6b74bbbf0cf83e485514c12c8a40c17539dbd881b48c1dc3db8ce33c0ee8d58d471529257b5fef5",
        "75d3b5bef49d923f4bca38879a7177b72ad64adbfe8edf837f3ead7c32d10d87cc0473840ca0679c633c63a576c7172b",
        "7eeec97cff00c8fbaa59c625c17224d776aef4f3b9d4e87e1cb1220b97cb34d318c6147ca7785ddefc76e9d6be0e155c",
        "9bec92baeead7f96bbb5a9fc319563a7282b6897e3a3dfa6da6dd5f73b4d2f46b10615546cbe58ef21b3b9189ea3238d",
        "c3823835db87af5250d5e8b45d366d7f5a7e17bfe919662aad2a10c545eaff00cbbee775a5784ac6fac22ba92e2e2369",
        "324ac476a0e48c01daba2799aa0f93d9a7a2ded7d55fb74bd8fb2a58aa9cbaa8bdf569df7f547fffd9",
    )),
    ("rgb444_rst", "bb6bbe8e884d5dcca14cf47b626f4e50fce1294444f73acc88e0ea9b132a37fd", concat!(
        "ffd8ffe000104a46494600010100000100010000ffdb0043000d090a0b0a080d0b0a0b0e0e0d0f13201513121213271c",
        "1e17202e2931302e292d2c333a4a3e333646372c2d405741464c4e525352323e5a615a50604a51524fffdb0043010e0e",
        "0e131113261515264f352d354f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f4f",
        "4f4f4f4f4f4f4f4f4f4f4f4f4f4fffc0001108000d001303011100021101031101ffc4001f0000010501010101010100",
        "000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d010203000411051221",
        "31410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a",
        "434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a9293949596979899",
        "9aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1",
        "f2f3f4f5f6f7f8f9faffc4001f0100030101010101010101010000000000000102030405060708090a0bffc400b51100",
        "020102040403040705040400010277000102031104052131061241510761711322328108144291a1b1c109233352f015",
        "6272d10a162434e125f11718191a262728292a35363738393a434445464748494a535455565758595a63646566676869",
        "6a737475767778797a82838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4",
        "c5c6c7c8c9cad2d3d4d5d6d7d8d9dae2e3e4e5e6e7e8e9eaf2f3f4f5f6f7f8f9faffdd00040002ffda000c0301000211",
        "0311003f00e52db4b02450ca4367f13fe7fcf5a852bab6e654315ccefd0d7b2d31900051987a91dbf2f7ad5493b5dff5",
        "fd7f563dbc3e32ed7975febfad4fffd08c5815e39fc0118add547d0fab8e2276d11a16d6317c8e792cd8e9d39c66bc95",
        "2bfa1f9661eab68fffd1e820b58b2a003cf273cf507ffaff009d72c24daf438684e5182a88d6834d8a5856467705bb2f",
        "0055bafc9a58f4a351dba7f5f33fffd9",
    )),
    ("gray", "e55db4dccb7b075cf755489d28cb1a2a70996b2628b8b2517dd52a157f918946", concat!(
        "ffd8ffe000104a46494600010100000100010000ffdb0043000302020302020303030304030304050805050404050a07",
        "0706080c0a0c0c0b0a0b0b0d0e12100d0e110e0b0b1016101113141515150c0f171816141812141514ffc0000b08000d",
        "001301011100ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b51000020103",
        "03020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f024336272",
        "82090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475",
        "767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9",
        "cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9faffda0008010100003f00f913c31f06a34d4a",
        "08ee2da449c4806e1932b2e1867681ebb403c039e786e7d8fc0ff0826b24851ad679d085c34a8dbb6e0e195761182180",
        "1c11c6719381e8e9f0d1ed57ca6f3548249589591573ce00cf18ce3f0af44f0afc38d35859de499792e6e0c230a06c61",
        "284dfce73f2f63c673c7381eb1a0f83b4c56b5548dc99373379a4383be362c3246e1c165e08e18f7e6bd7f40f849a6eb",
        "7a45bdf4d777504936e263b66091afcc400abce3815fffd9",
    )),
];

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn jpeg_matches_libjpeg_turbo() {
    for (name, want, hexd) in JPEGS {
        let im = super::jpeg::decode(&unhex(hexd)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(im.hash(), *want, "{name}");
    }
}
