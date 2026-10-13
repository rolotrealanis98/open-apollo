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
    (
        "rgb444",
        "614e8e85e2e38775c71c72ee9f5a7e5edf4e8bb3b7cd9839de1eee92491aeddb",
        concat!(
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
        ),
    ),
    (
        "rgb444_rst",
        "bb6bbe8e884d5dcca14cf47b626f4e50fce1294444f73acc88e0ea9b132a37fd",
        concat!(
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
        ),
    ),
    (
        "gray",
        "e55db4dccb7b075cf755489d28cb1a2a70996b2628b8b2517dd52a157f918946",
        concat!(
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
        ),
    ),
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

fn png_bytes(im: &Rgba, text: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, im.w, im.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        for (k, v) in text {
            enc.add_text_chunk(k.to_string(), v.to_string()).unwrap();
        }
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&im.px).unwrap();
    }
    out
}

#[test]
fn png_interleave_is_undone() {
    use super::decode::{Decode, decode};
    let a = synth(8, 6, 21, Alpha::Mixed);
    // stored row gy[i] holds output row i, gy = rows 0,2,4 then 1,3,5
    let gy = [0u32, 2, 4, 1, 3, 5];
    let mut stored = Rgba::new(8, 6);
    for (i, &g) in gy.iter().enumerate() {
        stored.paste(&a.crop(0, i as i64, 8, 1), 0, g as i64);
    }
    let b = png_bytes(&stored, &[("InterleaveY", "2")]);
    assert_eq!(decode(&b, Decode::Png).unwrap(), a);
    assert_eq!(decode(&b, Decode::PngRaw).unwrap(), stored);
}

fn bmp32(w: u32, h: u32, bgra: &[[u8; 4]]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(54 + bgra.len() as u32 * 4).to_le_bytes());
    b.extend_from_slice(&[0, 0, 0, 0]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(h as i32).to_le_bytes()); // bottom-up
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    for p in bgra {
        b.extend_from_slice(p);
    }
    b
}

#[test]
fn bmp_rules() {
    use super::decode::{Decode, decode};
    // bottom-up rows: the last pixel written is the top-right one
    let b = bmp32(2, 1, &[[64, 32, 16, 128], [10, 20, 30, 0]]);
    let im = decode(&b, Decode::Bmp).unwrap();
    assert_eq!(im.pixel(0, 0), [16, 32, 64, 255], "BI_RGB 32-bit ignores alpha");
    let im = decode(&b, Decode::BmpPremultiplied).unwrap();
    // 255/128 = 1.9921875 (exact in f32): 64 -> 127.5 -> 127, 32 -> 63.75 -> 63, 16 -> 31.875 -> 31
    assert_eq!(im.pixel(0, 0), [31, 63, 127, 128]);
    assert_eq!(im.pixel(1, 0), [0, 0, 0, 0]);
    let opaque = decode(&bmp32(1, 1, &[[1, 2, 3, 0]]), Decode::BmpPremultiplied).unwrap();
    assert_eq!(opaque.pixel(0, 0), [3, 2, 1, 255], "an all-zero alpha band means opaque");
}

#[test]
fn mpk1_is_zlib_bgra() {
    use super::decode::{Decode, decode};
    use std::io::Write;
    let raw = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&raw).unwrap();
    let z = z.finish().unwrap();
    let mut b = b"MPK1".to_vec();
    for v in [2u32, 1, 32, z.len() as u32] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&z);
    assert_eq!(decode(&b, Decode::Mpk1).unwrap().px, vec![3, 2, 1, 4, 7, 6, 5, 8]);
}

/// A whole build against a synthetic install: one fake Powered plug-in DLL whose
/// resources and bytes are synthetic, one recipe file.
#[test]
fn builds_a_synthetic_install() {
    use super::img::sha256;
    use super::{Options, Outcome, build_with};
    let t = std::env::temp_dir().join(format!("oa-assets-plugins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&t);
    let a = synth(9, 7, 31, Alpha::Mixed);
    let c = synth(12, 10, 32, Alpha::Opaque);
    let (pa, pc) = (png_bytes(&a, &[]), png_bytes(&c, &[]));
    let mut dll = super::pe::tests::tiny_pe_with(&pa, &pc);
    let carve_at = dll.windows(pc.len()).position(|w| w == pc.as_slice()).unwrap();
    dll[0x300..0x306].copy_from_slice(b"hello\0");
    let dir = t.join("win/Program Files/Universal Audio/Powered Plugins/UAD-2 Powered Plugins");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("UAD Test.dll"), &dll).unwrap();

    let want_res = resize(&a, 18, 14, Filter::Lanczos, None).hash();
    let want_carve = c.crop(2, 3, 5, 4).hash();
    let recipes = t.join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    let doc = serde_json::json!({
        "recipesSchema": 2, "group": "test", "tier": 0,
        "inputs": {"dll:UAD Test.dll": {"kind": "dll", "file": "UAD Test.dll", "root": "powered-plugins"}},
        "uses": ["t_res", "t_carve", "t_toolbar", "t_bad"],
        "sprites": {
            "t_res": {"status": "verified", "build": [
                {"op": "load", "in": "dll:UAD Test.dll", "res": "PNG/A.PNG/1033", "sha": sha256(&pa), "dec": "png"},
                {"op": "lanczos2x"}], "output": {"w": 18, "h": 14, "rgba": want_res}},
            "t_carve": {"status": "fallback", "build": [
                {"op": "load", "in": "dll:UAD Test.dll", "carve": [carve_at, pc.len()], "sha": sha256(&pc), "dec": "png"},
                {"op": "crop", "box": [2, 3, 5, 4]}], "output": {"w": 5, "h": 4, "rgba": want_carve}},
            "t_toolbar": {"status": "runtime", "owner": "plugin-ui/chrome", "kind": "chrome"},
            "t_bad": {"status": "verified", "build": [
                {"op": "load", "in": "dll:UAD Test.dll", "res": "10/42/1033", "sha": "00".repeat(32), "dec": "png"}],
                "output": {"w": 1, "h": 1}}
        },
        "texts": {"t.hello": {"status": "verified", "kind": "dll-string", "input": "dll:UAD Test.dll",
            "at": {"fileOffset": 0x300, "length": 16}, "encoding": "utf-8", "output": {"sha256": sha256(b"hello")}}}
    });
    std::fs::write(recipes.join("test.json"), doc.to_string()).unwrap();
    let out = t.join("out");
    let opts = Options { recipes: Some(recipes.clone()), jobs: 2 };
    let mut ticks = 0;
    let rep = build_with(&t.join("win"), &out, &[], &opts, &mut |_, _| ticks += 1).unwrap();
    let get = |id: &str| rep.sprites.iter().find(|s| s.id == id).unwrap().outcome.clone();
    assert_eq!(get("t_res"), Outcome::Ok);
    assert_eq!(get("t_carve"), Outcome::Ok);
    assert_eq!(get("t_toolbar"), Outcome::Skipped);
    assert!(matches!(get("t_bad"), Outcome::Failed(_)));
    assert_eq!(ticks, 3);
    assert!(out.join("Images/t_res@2x.png").is_file());
    assert!(!out.join("Images/t_bad@2x.png").exists());
    assert_eq!(std::fs::read(out.join("Text/t.hello.txt")).unwrap(), b"hello");
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(m["sprites"]["t_res"]["rgba"], want_res);
    assert_eq!(m["specs"]["test"]["ready"], false);
    assert_eq!(m["specs"]["test"]["runtime"], 1);
    // the written PNG decodes back to the same pixels
    let back =
        super::decode::decode(&std::fs::read(out.join("Images/t_carve@2x.png")).unwrap(), super::decode::Decode::Png);
    assert_eq!(back.unwrap().hash(), want_carve);

    // once the bad sprite is dropped from the spec, it is ready; a lazy --spec build keeps earlier images
    let mut doc = doc;
    doc["uses"] = serde_json::json!(["t_res", "t_carve", "t_toolbar"]);
    doc["sprites"].as_object_mut().unwrap().remove("t_bad");
    std::fs::write(recipes.join("test.json"), doc.to_string()).unwrap();
    let rep = build_with(&t.join("win"), &out, &["test".into()], &opts, &mut |_, _| {}).unwrap();
    assert!(rep.specs.iter().any(|s| s.id == "test" && s.ready));
    std::fs::remove_dir_all(&t).unwrap();
}

#[test]
fn colour_ops_match_reference() {
    // expected hashes: the Python prototypes the recipes were verified with, on the same synthetic pixels
    let src = synth(37, 23, 1, Alpha::Mixed);
    let mut t = src.clone();
    t.premul_tint([40, 200, 90], 150);
    assert_eq!(t.hash(), "2f1e598f13410f926c245dd8504f78fdd7b9e6e63194ae8f142aab036fb4986a");
    let mut t = src.clone();
    t.premul_tint([255, 0, 7], 256);
    assert_eq!(t.hash(), "e80a9bf88f52791d0a4e94b264fe8816b398bbd5fba13fe8f3ccb8c58a18c89f");
    let mut t = src.clone();
    t.juce_premul_roundtrip();
    assert_eq!(t.hash(), "ec4a9567a692ae0adfeae30d100005b2bdabb7ce19619a83549b41c15f424a1c");
}

/// The recipes compiled into the binary parse, define every id their specs use
/// once, and every `sprite`/`blob` reference resolves.
#[test]
fn builtin_recipes_are_consistent() {
    use super::recipe::{Op, Status, deps};
    let book = super::builtin_book().expect("built-in recipes load");
    if book.files.is_empty() {
        return; // a checkout without recipe data
    }
    for f in &book.files {
        for id in &f.uses {
            assert!(book.sprite(id).is_some(), "{}: uses unknown sprite {id}", f.group);
        }
        for (id, s) in &f.sprites {
            match s.status {
                Status::Verified | Status::Fallback => {
                    let b = s.build.as_ref().unwrap_or_else(|| panic!("{id}: no build"));
                    let o = s.output.as_ref().unwrap_or_else(|| panic!("{id}: no output"));
                    assert!(o.rgba.is_some(), "{id}: no expected hash");
                    let mut refs = Vec::new();
                    deps(b, &mut refs);
                    for r in refs {
                        assert!(book.sprite(&r).is_some_and(|(_, s)| s.status.buildable()), "{id}: bad ref {r}");
                    }
                    fn blobs(ops: &[Op], out: &mut Vec<String>) {
                        for op in ops {
                            match op {
                                Op::Load { blob: Some(k), .. } => out.push(k.clone()),
                                Op::Stack { parts, .. } => parts.iter().for_each(|p| blobs(p, out)),
                                Op::Paste { src, .. } => blobs(src, out),
                                Op::ThumbFader { track, thumb, .. } => {
                                    blobs(track, out);
                                    blobs(thumb, out);
                                }
                                Op::Slide { thumb, .. } => blobs(thumb, out),
                                Op::Layers { layers, .. } => layers.iter().for_each(|l| blobs(&l.src, out)),
                                _ => {}
                            }
                        }
                    }
                    let mut ks = Vec::new();
                    blobs(b, &mut ks);
                    for k in ks {
                        let bl = f.blobs.get(&k).unwrap_or_else(|| panic!("{id}: unknown blob {k}"));
                        assert!(f.inputs.contains_key(&bl.input), "{id}: blob input {} not declared", bl.input);
                    }
                }
                Status::Runtime | Status::Deferred => assert!(s.kind.is_some(), "{id}: no kind"),
                Status::NoRecipe => {}
            }
        }
    }
}
