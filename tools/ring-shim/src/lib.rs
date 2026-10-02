pub mod error {
    #[derive(Debug)]
    pub struct Unspecified;
}

pub mod aead {
    use crate::error::Unspecified;
    use chacha20poly1305::{AeadInPlace, ChaCha20Poly1305, KeyInit};

    pub struct Algorithm;

    pub static CHACHA20_POLY1305: Algorithm = Algorithm;

    pub struct UnboundKey(ChaCha20Poly1305);

    impl UnboundKey {
        pub fn new(_algorithm: &'static Algorithm, key: &[u8]) -> Result<UnboundKey, Unspecified> {
            Ok(UnboundKey(
                ChaCha20Poly1305::new_from_slice(key).map_err(|_| Unspecified)?,
            ))
        }
    }

    pub struct LessSafeKey(ChaCha20Poly1305);

    impl LessSafeKey {
        pub fn new(key: UnboundKey) -> LessSafeKey {
            LessSafeKey(key.0)
        }

        pub fn seal_in_place_separate_tag<A: AsRef<[u8]>>(
            &self,
            nonce: Nonce,
            aad: Aad<A>,
            in_out: &mut [u8],
        ) -> Result<Tag, Unspecified> {
            let n = chacha20poly1305::Nonce::from(nonce.0);
            let tag = self
                .0
                .encrypt_in_place_detached(&n, aad.0.as_ref(), in_out)
                .map_err(|_| Unspecified)?;
            Ok(Tag(tag.into()))
        }

        pub fn open_in_place<'io, A: AsRef<[u8]>>(
            &self,
            nonce: Nonce,
            aad: Aad<A>,
            in_out: &'io mut [u8],
        ) -> Result<&'io mut [u8], Unspecified> {
            if in_out.len() < 16 {
                return Err(Unspecified);
            }
            let (buf, tag) = in_out.split_at_mut(in_out.len() - 16);
            let n = chacha20poly1305::Nonce::from(nonce.0);
            let mut tag_buf = [0u8; 16];
            tag_buf.copy_from_slice(tag);
            self.0
                .decrypt_in_place_detached(&n, aad.0.as_ref(), buf, chacha20poly1305::Tag::from_slice(&tag_buf))
                .map_err(|_| Unspecified)?;
            Ok(buf)
        }
    }

    pub struct Nonce([u8; 12]);

    impl Nonce {
        pub fn assume_unique_for_key(nonce: [u8; 12]) -> Nonce {
            Nonce(nonce)
        }
    }

    // 与 ring 0.16 同形：泛型 + 固有 from 构造器（Aad::from(&[]) 因此成立）
    pub struct Aad<A: AsRef<[u8]>>(A);

    impl<A: AsRef<[u8]>> Aad<A> {
        #[inline]
        pub fn from(aad: A) -> Self {
            Aad(aad)
        }
    }

    pub struct Tag([u8; 16]);

    impl AsRef<[u8]> for Tag {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }
}

pub mod constant_time {
    use crate::error::Unspecified;
    use subtle::ConstantTimeEq;

    pub fn verify_slices_are_equal(a: &[u8], b: &[u8]) -> Result<(), Unspecified> {
        if a.len() == b.len() && bool::from(a.ct_eq(b)) {
            Ok(())
        } else {
            Err(Unspecified)
        }
    }
}
