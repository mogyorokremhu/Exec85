//! Password hashing: SHA-512 "crypt" (the `$6$salt$hash` format in /etc/shadow), written
//! from the public specification (Ulrich Drepper, "Unix crypt using SHA-256 and SHA-512").
//! Pure Rust so it needs nothing from the C library and is checked against the official test vector.

const H0: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];
const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

struct Sha512 { h: [u64; 8], buf: Vec<u8>, len: u128 }

impl Sha512 {
    fn new() -> Sha512 { Sha512 { h: H0, buf: Vec::with_capacity(128), len: 0 } }

    fn block(h: &mut [u64; 8], b: &[u8]) {
        let mut w = [0u64; 80];
        for i in 0..16 { w[i] = u64::from_be_bytes(b[i * 8..i * 8 + 8].try_into().unwrap()); }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b_, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b_) ^ (a & c) ^ (b_ & c);
            let t2 = s0.wrapping_add(maj);
            hh = g; g = f; f = e; e = d.wrapping_add(t1); d = c; c = b_; b_ = a; a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b_, c, d, e, f, g, hh]) { *x = x.wrapping_add(y); }
    }

    fn update(&mut self, data: &[u8]) {
        self.len += data.len() as u128;
        self.buf.extend_from_slice(data);
        let full = self.buf.len() / 128 * 128;
        for chunk in self.buf[..full].chunks(128) { Self::block(&mut self.h, chunk); }
        self.buf.drain(..full);
    }

    fn finish(mut self) -> [u8; 64] {
        let bits = self.len * 8;
        self.buf.push(0x80);
        while self.buf.len() % 128 != 112 { self.buf.push(0); }
        self.buf.extend_from_slice(&bits.to_be_bytes());
        for chunk in self.buf.chunks(128) { Self::block(&mut self.h, chunk); }
        let mut out = [0u8; 64];
        for (i, v) in self.h.iter().enumerate() { out[i * 8..i * 8 + 8].copy_from_slice(&v.to_be_bytes()); }
        out
    }
}

#[cfg(test)]
pub fn sha512(data: &[u8]) -> [u8; 64] { let mut s = Sha512::new(); s.update(data); s.finish() }

const ITOA64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn b64(out: &mut String, b2: u8, b1: u8, b0: u8, n: usize) {
    let mut w = ((b2 as u32) << 16) | ((b1 as u32) << 8) | b0 as u32;
    for _ in 0..n { out.push(ITOA64[(w & 0x3f) as usize] as char); w >>= 6; }
}

/// Repeat `src` (64 bytes) until `len` bytes are produced.
fn repeat_to(src: &[u8; 64], len: usize) -> Vec<u8> {
    (0..len).map(|i| src[i % 64]).collect()
}

/// Hash `password` with `salt` (at most 16 characters) and `rounds` (default 5000): "$6$salt$hash".
pub fn sha512_crypt(password: &[u8], salt: &str, rounds: Option<u32>) -> String {
    let salt = &salt.as_bytes()[..salt.len().min(16)];
    let (custom, rounds) = match rounds { Some(r) => (true, r.clamp(1000, 999_999_999)), None => (false, 5000) };

    let mut ctx_b = Sha512::new();
    ctx_b.update(password); ctx_b.update(salt); ctx_b.update(password);
    let b = ctx_b.finish();

    let mut a = Sha512::new();
    a.update(password); a.update(salt);
    a.update(&repeat_to(&b, password.len()));
    let mut cnt = password.len();
    while cnt > 0 { if cnt & 1 != 0 { a.update(&b); } else { a.update(password); } cnt >>= 1; }
    let mut a_digest = a.finish();

    let mut dp = Sha512::new();
    for _ in 0..password.len() { dp.update(password); }
    let p_bytes = repeat_to(&dp.finish(), password.len());

    let mut ds = Sha512::new();
    for _ in 0..(16 + a_digest[0] as usize) { ds.update(salt); }
    let s_bytes = repeat_to(&ds.finish(), salt.len());

    for i in 0..rounds {
        let mut c = Sha512::new();
        if i % 2 != 0 { c.update(&p_bytes); } else { c.update(&a_digest); }
        if i % 3 != 0 { c.update(&s_bytes); }
        if i % 7 != 0 { c.update(&p_bytes); }
        if i % 2 != 0 { c.update(&a_digest); } else { c.update(&p_bytes); }
        a_digest = c.finish();
    }

    let d = &a_digest;
    let mut out = String::from("$6$");
    if custom { out.push_str(&format!("rounds={rounds}$")); }
    out.push_str(std::str::from_utf8(salt).unwrap_or(""));
    out.push('$');
    const ORDER: [(usize, usize, usize); 21] = [
        (0, 21, 42), (22, 43, 1), (44, 2, 23), (3, 24, 45), (25, 46, 4), (47, 5, 26), (6, 27, 48),
        (28, 49, 7), (50, 8, 29), (9, 30, 51), (31, 52, 10), (53, 11, 32), (12, 33, 54), (34, 55, 13),
        (56, 14, 35), (15, 36, 57), (37, 58, 16), (59, 17, 38), (18, 39, 60), (40, 61, 19), (62, 20, 41),
    ];
    for (x, y, z) in ORDER { b64(&mut out, d[x], d[y], d[z], 4); }
    b64(&mut out, 0, 0, d[63], 2);
    out
}

/// A fresh random salt (16 characters from the crypt alphabet), read from the kernel's random source.
pub fn random_salt() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") { let _ = f.read_exact(&mut bytes); }
    bytes.iter().map(|b| ITOA64[(*b & 63) as usize] as char).collect()
}

/// New shadow-file hash for a password.
pub fn hash_password(password: &str) -> String { sha512_crypt(password.as_bytes(), &random_salt(), None) }

/// Check a password against a stored `$6$...` hash (other formats are rejected, not accepted).
pub fn verify(password: &str, stored: &str) -> bool {
    let Some(rest) = stored.strip_prefix("$6$") else { return false };
    let (rounds, rest) = match rest.strip_prefix("rounds=") {
        Some(r) => match r.split_once('$') { Some((n, rest)) => (n.parse::<u32>().ok(), rest), None => return false },
        None => (None, rest),
    };
    let Some((salt, _)) = rest.split_once('$') else { return false };
    let computed = sha512_crypt(password.as_bytes(), salt, rounds);
    constant_time_eq(computed.as_bytes(), stored.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

    #[test]
    fn sha512_known_answers() {
        assert_eq!(hex(&sha512(b"")), "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e");
        assert_eq!(hex(&sha512(b"abc")), "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f");
        // crosses a block boundary (112+ bytes needs a second padding block)
        let long = vec![b'a'; 1000];
        assert_eq!(hex(&sha512(&long)), "67ba5535a46e3f86dbfbed8cbbaf0125c76ed549ff8b0b9e03e0c88cf90fa634fa7b12b47d77b694de488ace8d9a65967dc96df599727d3292a8d9d447709c97");
    }

    #[test]
    fn matches_the_official_sha_crypt_test_vector() {
        // From the specification (Drepper): password "Hello world!", salt "saltstring".
        assert_eq!(
            sha512_crypt(b"Hello world!", "saltstring", None),
            "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
        );
        assert_eq!(
            sha512_crypt(b"Hello world!", "saltstringsaltstring", Some(10000)),
            "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v."
        );
    }

    #[test]
    fn verify_accepts_only_the_right_password() {
        let h = hash_password("correct horse");
        assert!(h.starts_with("$6$") && h.len() > 90);
        assert!(verify("correct horse", &h));
        assert!(!verify("correct horsf", &h));
        assert!(!verify("", &h));
        assert!(!verify("x", "!"), "locked accounts never verify");
        assert!(!verify("x", "$1$abc$def"), "other hash formats are rejected");
        assert_ne!(hash_password("same"), hash_password("same"), "random salts differ");
    }
}
