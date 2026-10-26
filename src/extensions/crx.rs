//! CRX3 packages: parse, verify, and take the archive out.
//!
//! A `.crx` is `Cr24`, a version, a protobuf header and then a plain ZIP. The
//! header carries proofs — a public key and a signature over the archive —
//! and the extension's id is derived from the key, which is what makes an id
//! meaningful: an attacker who swaps the payload cannot keep the id without
//! also holding the private key.
//!
//! **Nothing here trusts bytes it has not checked.** `verify` is the only way
//! to get at the archive, so a caller cannot install an unverified package by
//! forgetting a step (AGENTS §6). The RSA check runs through Windows CNG
//! rather than a hand-rolled or newly-added crypto library.

use sha2::{Digest, Sha256};

use super::web_store::ExtensionId;
use super::ExtensionError;

/// Prefix of the data a CRX3 signature is taken over. The archive alone is
/// not signed: the signed payload names the format, so a signature cannot be
/// lifted out of a CRX and replayed over something else.
const SIGNED_PREFIX: &[u8] = b"CRX3 SignedData\x00";

/// Field numbers from `crx3.proto`. Written out because a bare 2 or 10000 in
/// the parser below says nothing about what it selects.
mod field {
    /// `CrxFileHeader.sha256_with_rsa`, repeated `AsymmetricKeyProof`.
    pub const SHA256_WITH_RSA: u64 = 2;
    /// `CrxFileHeader.signed_header_data`, bytes — itself a `SignedData`.
    pub const SIGNED_HEADER_DATA: u64 = 10000;
    /// `AsymmetricKeyProof.public_key` and `.signature`, both bytes.
    pub const PUBLIC_KEY: u64 = 1;
    pub const SIGNATURE: u64 = 2;
    /// `SignedData.crx_id`, 16 bytes.
    pub const CRX_ID: u64 = 1;
}

/// A CRX whose signature has been checked. Holding one of these is the proof:
/// the archive is only reachable through `verify`.
#[derive(Debug)]
pub struct VerifiedCrx {
    id: ExtensionId,
    archive: Vec<u8>,
}

impl VerifiedCrx {
    /// The id the package's own signing key says it is — not the id that was
    /// asked for. A caller that requested a specific extension must compare.
    pub fn id(&self) -> &ExtensionId {
        &self.id
    }

    pub fn archive(&self) -> &[u8] {
        &self.archive
    }
}

/// Check a `.crx` and hand back its archive.
///
/// Every one of these must hold, and a failure of any is a refusal rather
/// than a warning:
///
/// 1. `Cr24`, version 3.
/// 2. At least one `sha256_with_rsa` proof whose signature verifies over
///    `"CRX3 SignedData\0" || len(signed_header_data) || signed_header_data
///    || archive` — the whole archive, so a single changed byte fails.
/// 3. The declared `crx_id` equals the first 16 bytes of the SHA-256 of the
///    public key that signed it. Without this a valid signature from *any*
///    key would pass, which is the entire attack.
pub fn verify(bytes: &[u8]) -> Result<VerifiedCrx, ExtensionError> {
    let (header, archive) = split(bytes)?;
    let mut signed_header_data = None;
    let mut proofs = Vec::new();
    for field in Reader::new(header) {
        let (number, value) = field?;
        match number {
            field::SHA256_WITH_RSA => proofs.push(proof(value)?),
            field::SIGNED_HEADER_DATA => signed_header_data = Some(value),
            _ => {}
        }
    }
    let signed_header_data =
        signed_header_data.ok_or_else(|| ExtensionError::Crx("no signed header".into()))?;

    // The id the package claims, before any signature is believed.
    let declared = crx_id(signed_header_data)?;

    let mut payload = Vec::with_capacity(SIGNED_PREFIX.len() + 4 + signed_header_data.len());
    payload.extend_from_slice(SIGNED_PREFIX);
    payload.extend_from_slice(&(signed_header_data.len() as u32).to_le_bytes());
    payload.extend_from_slice(signed_header_data);
    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(&payload);
        hasher.update(archive);
        hasher.finalize()
    };

    for (public_key, signature) in &proofs {
        // Key first: a signature that verifies under a key which does not
        // produce this id proves nothing about this extension.
        if id_from_key(public_key) != declared {
            continue;
        }
        if verify_rsa(public_key, &digest, signature)? {
            return Ok(VerifiedCrx {
                id: ExtensionId::parse(&id_text(&declared))?,
                archive: archive.to_vec(),
            });
        }
    }
    Err(ExtensionError::Crx(
        "no signature from the key this extension's id is derived from".into(),
    ))
}

/// Split the container. The header length is attacker-controlled, so it is
/// checked against the real length rather than used to slice.
fn split(bytes: &[u8]) -> Result<(&[u8], &[u8]), ExtensionError> {
    let fixed = bytes
        .get(..12)
        .ok_or_else(|| ExtensionError::Crx("shorter than a CRX header".into()))?;
    if &fixed[..4] != b"Cr24" {
        return Err(ExtensionError::Crx("not a CRX package".into()));
    }
    let version = u32::from_le_bytes(fixed[4..8].try_into().expect("four bytes"));
    if version != 3 {
        return Err(ExtensionError::Crx(format!(
            "CRX version {version}, and only 3 is supported"
        )));
    }
    let header_len = u32::from_le_bytes(fixed[8..12].try_into().expect("four bytes")) as usize;
    let rest = &bytes[12..];
    let header = rest
        .get(..header_len)
        .ok_or_else(|| ExtensionError::Crx("header runs past the end of the file".into()))?;
    Ok((header, &rest[header_len..]))
}

/// One `AsymmetricKeyProof`: a key and a signature, both required.
fn proof(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>), ExtensionError> {
    let mut public_key = None;
    let mut signature = None;
    for field in Reader::new(bytes) {
        let (number, value) = field?;
        match number {
            field::PUBLIC_KEY => public_key = Some(value.to_vec()),
            field::SIGNATURE => signature = Some(value.to_vec()),
            _ => {}
        }
    }
    match (public_key, signature) {
        (Some(k), Some(s)) => Ok((k, s)),
        _ => Err(ExtensionError::Crx(
            "a proof is missing a key or a signature".into(),
        )),
    }
}

/// `SignedData.crx_id` — exactly 16 bytes, or the package is malformed.
fn crx_id(signed_header_data: &[u8]) -> Result<[u8; 16], ExtensionError> {
    for field in Reader::new(signed_header_data) {
        let (number, value) = field?;
        if number == field::CRX_ID {
            return value
                .try_into()
                .map_err(|_| ExtensionError::Crx("crx_id is not 16 bytes".into()));
        }
    }
    Err(ExtensionError::Crx("no crx_id in the signed header".into()))
}

/// The id bytes a public key produces: the first 16 bytes of its SHA-256.
fn id_from_key(public_key: &[u8]) -> [u8; 16] {
    let digest = Sha256::digest(public_key);
    digest[..16].try_into().expect("sha256 is 32 bytes")
}

/// Chromium's rendering of id bytes: each nibble as a letter from `a`, so
/// `0` is `a` and `f` is `p`. Not hex, though it is a hex alphabet shifted.
fn id_text(id: &[u8; 16]) -> String {
    let mut out = String::with_capacity(32);
    for byte in id {
        out.push((b'a' + (byte >> 4)) as char);
        out.push((b'a' + (byte & 0x0f)) as char);
    }
    out
}

/// RSA PKCS#1 v1.5 over SHA-256, through Windows CNG. The key arrives as
/// SubjectPublicKeyInfo DER, which is what `CryptDecodeObjectEx` takes.
///
/// A bad signature is `Ok(false)`, not an error: it is an expected answer.
/// `Err` is reserved for a key the OS would not even parse.
#[cfg(target_os = "windows")]
fn verify_rsa(public_key: &[u8], digest: &[u8], signature: &[u8]) -> Result<bool, ExtensionError> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        BCryptDestroyKey, BCryptVerifySignature, CryptDecodeObjectEx, CryptImportPublicKeyInfoEx2,
        BCRYPT_KEY_HANDLE, BCRYPT_PAD_PKCS1, BCRYPT_PKCS1_PADDING_INFO, BCRYPT_SHA256_ALGORITHM,
        CERT_PUBLIC_KEY_INFO, CRYPT_DECODE_ALLOC_FLAG, CRYPT_IMPORT_PUBLIC_KEY_FLAGS,
        X509_ASN_ENCODING, X509_PUBLIC_KEY_INFO,
    };

    /// `CRYPT_DECODE_ALLOC_FLAG` means crypt32 allocated with `LocalAlloc`,
    /// so every path out of the block below has to give it back.
    struct LocalGuard(*mut std::ffi::c_void);
    impl Drop for LocalGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0)));
            }
        }
    }

    unsafe {
        let mut decoded: *mut CERT_PUBLIC_KEY_INFO = std::ptr::null_mut();
        let mut decoded_len = 0u32;
        CryptDecodeObjectEx(
            X509_ASN_ENCODING,
            X509_PUBLIC_KEY_INFO,
            public_key,
            CRYPT_DECODE_ALLOC_FLAG,
            None,
            Some(&mut decoded as *mut _ as *mut std::ffi::c_void),
            &mut decoded_len,
        )
        .map_err(|e| ExtensionError::Crx(format!("public key is not readable: {e}")))?;
        // Freed on every path below, including the early returns.
        let _guard = LocalGuard(decoded as *mut std::ffi::c_void);

        let mut key = BCRYPT_KEY_HANDLE::default();
        CryptImportPublicKeyInfoEx2(
            X509_ASN_ENCODING,
            decoded,
            CRYPT_IMPORT_PUBLIC_KEY_FLAGS(0),
            None,
            &mut key,
        )
        .map_err(|e| ExtensionError::Crx(format!("public key could not be imported: {e}")))?;

        let padding = BCRYPT_PKCS1_PADDING_INFO {
            pszAlgId: BCRYPT_SHA256_ALGORITHM,
        };
        let status = BCryptVerifySignature(
            key,
            Some(&padding as *const _ as *const std::ffi::c_void),
            digest,
            signature,
            BCRYPT_PAD_PKCS1,
        );
        let _ = BCryptDestroyKey(key);
        Ok(status.is_ok())
    }
}

/// Off Windows there is no extension loading at all, so there is nothing a
/// verified package could be used for. Refusing is honest; a stub that
/// returned `true` would be a hole waiting for the day loading arrives.
#[cfg(not(target_os = "windows"))]
fn verify_rsa(
    _public_key: &[u8],
    _digest: &[u8],
    _signature: &[u8],
) -> Result<bool, ExtensionError> {
    Err(ExtensionError::Crx(
        "signature checking is implemented on Windows only".into(),
    ))
}

/// The scrap of protobuf this format needs: length-delimited and varint
/// fields, with everything else skipped. Not a general decoder — it reads
/// exactly what `crx3.proto` uses and refuses anything it cannot account for,
/// because a parser that guesses on malformed input is a parser that can be
/// steered.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.bytes.get(self.at)?;
            self.at += 1;
            value |= u64::from(byte & 0x7f).checked_shl(shift)?;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Result<(u64, &'a [u8]), ExtensionError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.bytes.len() {
            return None;
        }
        let malformed = || Some(Err(ExtensionError::Crx("malformed CRX header".into())));
        let Some(tag) = self.varint() else {
            return malformed();
        };
        let (number, wire) = (tag >> 3, tag & 0x07);
        match wire {
            // Length-delimited: the only kind this format carries payloads in.
            2 => {
                let Some(len) = self.varint() else {
                    return malformed();
                };
                let end = self.at.checked_add(len as usize)?;
                let Some(value) = self.bytes.get(self.at..end) else {
                    return malformed();
                };
                self.at = end;
                Some(Ok((number, value)))
            }
            // Varint: skipped, but it still has to be well formed.
            0 => match self.varint() {
                Some(_) => Some(Ok((number, &[]))),
                None => malformed(),
            },
            // Fixed widths are not used by crx3.proto; anything else is a
            // format we do not know, and guessing its length would desync.
            5 | 1 => {
                let width = if wire == 5 { 4 } else { 8 };
                let end = self.at.checked_add(width)?;
                if end > self.bytes.len() {
                    return malformed();
                }
                self.at = end;
                Some(Ok((number, &[])))
            }
            _ => malformed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a length-delimited protobuf field.
    fn bytes_field(number: u64, value: &[u8]) -> Vec<u8> {
        let mut out = varint((number << 3) | 2);
        out.extend(varint(value.len() as u64));
        out.extend_from_slice(value);
        out
    }

    fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    /// A CRX whose header is well formed but whose proof will not verify —
    /// enough to exercise the container and the id rules.
    fn crx(id: [u8; 16], public_key: &[u8], signature: &[u8], archive: &[u8]) -> Vec<u8> {
        let signed_header = bytes_field(field::CRX_ID, &id);
        let mut proof = bytes_field(field::PUBLIC_KEY, public_key);
        proof.extend(bytes_field(field::SIGNATURE, signature));
        let mut header = bytes_field(field::SHA256_WITH_RSA, &proof);
        header.extend(bytes_field(field::SIGNED_HEADER_DATA, &signed_header));

        let mut out = Vec::new();
        out.extend_from_slice(b"Cr24");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(archive);
        out
    }

    #[test]
    fn the_container_is_checked_before_anything_in_it() {
        assert!(matches!(verify(b""), Err(ExtensionError::Crx(_))));
        assert!(matches!(verify(b"Cr24"), Err(ExtensionError::Crx(_))));
        // Right magic, wrong version.
        let mut wrong_version = Vec::from(*b"Cr24");
        wrong_version.extend_from_slice(&2u32.to_le_bytes());
        wrong_version.extend_from_slice(&0u32.to_le_bytes());
        let message = verify(&wrong_version).unwrap_err().to_string();
        assert!(message.contains("version 2"), "{message}");
        // Not a CRX at all.
        assert!(verify(b"PK\x03\x04and then some").is_err());
    }

    #[test]
    fn a_header_length_past_the_end_is_refused_not_sliced() {
        let mut lying = Vec::from(*b"Cr24");
        lying.extend_from_slice(&3u32.to_le_bytes());
        lying.extend_from_slice(&0xffff_u32.to_le_bytes());
        lying.extend_from_slice(b"short");
        let message = verify(&lying).unwrap_err().to_string();
        assert!(message.contains("past the end"), "{message}");
    }

    #[test]
    fn a_signature_under_a_key_that_does_not_make_this_id_is_refused() {
        // The id says one thing; the key hashes to another. Even a perfect
        // signature must not satisfy this — it is the whole attack.
        let key = b"not a real key, but it hashes to something";
        let mistmatched = [0u8; 16];
        assert_ne!(id_from_key(key), mistmatched);
        let package = crx(mistmatched, key, b"signature", b"PK\x03\x04archive");
        let message = verify(&package).unwrap_err().to_string();
        assert!(message.contains("no signature from the key"), "{message}");
    }

    #[test]
    fn a_package_with_no_signed_header_is_refused() {
        // A complete proof, so the refusal is about the missing signed
        // header and not about the proof itself.
        let mut proof = bytes_field(field::PUBLIC_KEY, b"key");
        proof.extend(bytes_field(field::SIGNATURE, b"sig"));
        let header = bytes_field(field::SHA256_WITH_RSA, &proof);
        let mut package = Vec::from(*b"Cr24");
        package.extend_from_slice(&3u32.to_le_bytes());
        package.extend_from_slice(&(header.len() as u32).to_le_bytes());
        package.extend_from_slice(&header);
        let message = verify(&package).unwrap_err().to_string();
        assert!(message.contains("no signed header"), "{message}");
    }

    #[test]
    fn a_proof_missing_half_of_itself_is_refused() {
        let signed_header = bytes_field(field::CRX_ID, &[0u8; 16]);
        let proof = bytes_field(field::PUBLIC_KEY, b"key only");
        let mut header = bytes_field(field::SHA256_WITH_RSA, &proof);
        header.extend(bytes_field(field::SIGNED_HEADER_DATA, &signed_header));
        let mut package = Vec::from(*b"Cr24");
        package.extend_from_slice(&3u32.to_le_bytes());
        package.extend_from_slice(&(header.len() as u32).to_le_bytes());
        package.extend_from_slice(&header);
        let message = verify(&package).unwrap_err().to_string();
        assert!(
            message.contains("missing a key or a signature"),
            "{message}"
        );
    }

    #[test]
    fn a_crx_id_of_the_wrong_length_is_refused() {
        let package = crx_with_id_bytes(&[0u8; 15]);
        let message = verify(&package).unwrap_err().to_string();
        assert!(message.contains("not 16 bytes"), "{message}");
    }

    fn crx_with_id_bytes(id: &[u8]) -> Vec<u8> {
        let signed_header = bytes_field(field::CRX_ID, id);
        let mut proof = bytes_field(field::PUBLIC_KEY, b"key");
        proof.extend(bytes_field(field::SIGNATURE, b"sig"));
        let mut header = bytes_field(field::SHA256_WITH_RSA, &proof);
        header.extend(bytes_field(field::SIGNED_HEADER_DATA, &signed_header));
        let mut package = Vec::from(*b"Cr24");
        package.extend_from_slice(&3u32.to_le_bytes());
        package.extend_from_slice(&(header.len() as u32).to_le_bytes());
        package.extend_from_slice(&header);
        package
    }

    #[test]
    fn truncated_protobuf_fields_are_refused_rather_than_guessed() {
        let mut package = Vec::from(*b"Cr24");
        package.extend_from_slice(&3u32.to_le_bytes());
        // A field claiming 200 bytes inside a 3-byte header.
        let header = [(field::SHA256_WITH_RSA << 3 | 2) as u8, 200, 0];
        package.extend_from_slice(&(header.len() as u32).to_le_bytes());
        package.extend_from_slice(&header);
        assert!(verify(&package).is_err());
    }

    #[test]
    fn ids_render_in_chromiums_shifted_alphabet() {
        assert_eq!(id_text(&[0u8; 16]), "a".repeat(32));
        assert_eq!(id_text(&[0xff; 16]), "p".repeat(32));
        let mut id = [0u8; 16];
        id[0] = 0x0f;
        assert!(id_text(&id).starts_with("ap"));
        // Every rendered id must satisfy the id parser, or the two
        // definitions have drifted apart.
        assert!(ExtensionId::parse(&id_text(&[0x5a; 16])).is_ok());
    }

    #[test]
    fn the_signed_payload_names_the_format_so_a_signature_cannot_be_replayed() {
        // Guards the constant itself: Chromium's verifier uses this exact
        // prefix, and a change here silently rejects every real package.
        assert_eq!(SIGNED_PREFIX, b"CRX3 SignedData\0");
    }
}
