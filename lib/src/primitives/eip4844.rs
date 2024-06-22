// #![cfg(feature = "kzg")]

use core::fmt::Display;
use std::sync::{Arc, RwLock};
use once_cell::sync::Lazy;
use revm_primitives::{kzg::{G1Points, G2Points, G1_POINTS, G2_POINTS}, B256};
use sha2::{Digest as _, Sha256};
use kzg::eip_4844::{
    compute_challenge, compute_kzg_proof_rust,
    blob_to_polynomial, evaluate_polynomial_in_evaluation_form, hash_to_bls_field, Blob
};
use kzg::{G1, Fr};
use crate::input::GuestInput;

#[cfg(feature = "kzg-zkcrypto")]
mod backend_exports {
    pub use rust_kzg_zkcrypto::kzg_proofs::KZGSettings as TaikoKzgSettings;
    pub use rust_kzg_zkcrypto::eip_4844::deserialize_blob_rust;
    pub use kzg::eip_4844::blob_to_kzg_commitment_rust;
}
pub use backend_exports::*;

pub const VERSIONED_HASH_VERSION_KZG: u8 = 0x01;
pub static MAINNET_KZG_TRUSTED_SETUP: Lazy<Arc<TaikoKzgSettings>> = 
    Lazy::new(|| {
        Arc::new(
            kzg::eip_4844::load_trusted_setup_rust(
                G1Points::as_ref(&G1_POINTS).flatten(), 
                G2Points::as_ref(&G2_POINTS).flatten()
            )
            .expect("failed to load trusted setup"),
        )
    });

pub static mut VERSION_HASH_AND_PROOF: Lazy<RwLock<(B256, KzgGroup)>> = 
    Lazy::new(|| RwLock::new((B256::default(), [0u8; 48].into())));


pub type KzgGroup = [u8; 48];
pub type KzgField = [u8; 32];

#[derive(Debug, thiserror::Error)]
pub enum Eip4844Error {
    #[error("Failed to deserialize blob to field elements")]
    DeserializeBlob,
    #[error("Failed to evaluate polynomial at hashed point: {0}")]
    EvaluatePolynomial(String),
    #[error("Failed to compute KZG proof")]
    ComputeKzgProof(String),
    #[error("Failed set commitment proof")]
    SetCommitmentProof(String),
}

pub fn proof_of_equivalence(input: &GuestInput) -> Result<Option<KzgField>, Eip4844Error> {
    if input.taiko.skip_verify_blob {
        return Ok(None);
    } else {
        let blob = &input.taiko.tx_data;
        let kzg_settings = input.taiko.kzg_settings.as_ref().unwrap_or_else(|| {
            // very costly, should not happen
            println!("initializing kzg settings in prover"); 
            &*MAINNET_KZG_TRUSTED_SETUP
        });
        Ok(Some(proof_of_equivalence_eval(blob, kzg_settings)?))
    }
}

pub fn proof_of_version_hash(input: &GuestInput) -> Result<Option<B256>, Eip4844Error> {
    if input.taiko.skip_verify_blob {
        return Ok(None);
    } else {
        let blob_fields = Blob::from_bytes(&input.taiko.tx_data)
            .map(|b| deserialize_blob_rust(&b))
            .flatten()
            .map_err(|_| Eip4844Error::DeserializeBlob)?;

        let kzg_settings = input.taiko.kzg_settings.as_ref().unwrap_or_else(|| &*MAINNET_KZG_TRUSTED_SETUP);
        let commitment = blob_to_kzg_commitment_rust(&blob_fields, kzg_settings)
            .map_err(|e| Eip4844Error::ComputeKzgProof(e))?;
        Ok(Some(commitment_to_version_hash(&commitment.to_bytes())))
    }
}

pub fn proof_of_equivalence_eval(blob: &[u8], kzg_settings: &TaikoKzgSettings) -> Result<KzgField, Eip4844Error> {

    let blob_fields = Blob::from_bytes(blob)
        .map(|b| deserialize_blob_rust(&b))
        .flatten()
        .map_err(|_| Eip4844Error::DeserializeBlob)?;

    let poly = blob_to_polynomial(&blob_fields).unwrap();
    let blob_hash = Sha256::digest(blob).into();
    let x = hash_to_bls_field(&blob_hash);
    
    // y = poly(x)
    evaluate_polynomial_in_evaluation_form(&poly, &x, kzg_settings)
        .map(|fr| fr.to_bytes())
        .map_err(|e| Eip4844Error::EvaluatePolynomial(e))
}

pub fn get_kzg_proof_commitment(blob: &[u8], kzg_settings: &TaikoKzgSettings) -> Result<(KzgGroup, KzgGroup), Eip4844Error> {
    let blob_fields = Blob::from_bytes(blob)
        .map(|b| deserialize_blob_rust(&b))
        .flatten()
        .map_err(|_| Eip4844Error::DeserializeBlob)?;

    let commitment = blob_to_kzg_commitment_rust(&blob_fields, kzg_settings)
        .map_err(|e| Eip4844Error::ComputeKzgProof(e))?;

    let evaluation_challenge_fr = compute_challenge(&blob_fields, &commitment);
    let (proof, _) = compute_kzg_proof_rust(&blob_fields, &evaluation_challenge_fr, kzg_settings)
        .map_err(|e| Eip4844Error::ComputeKzgProof(e))?;

    Ok((proof.to_bytes(), commitment.to_bytes()))
}


pub fn set_commitment_proof(proof: &KzgGroup, commitment: &KzgGroup) -> Result<(), Eip4844Error> {
    let version_hash = commitment_to_version_hash(&commitment);
    unsafe {
        *VERSION_HASH_AND_PROOF
            .write()
            .map_err(|e| Eip4844Error::SetCommitmentProof(e.to_string()))?
        = (version_hash, *proof);
    }
    Ok(())
}

pub fn commitment_to_version_hash(commitment: &KzgGroup) -> B256 {
    let mut hash = Sha256::digest(commitment);
    hash[0] = VERSIONED_HASH_VERSION_KZG;
    B256::new(hash.into())
}



#[cfg(test)]
mod test {
    use super::*;
    use kzg::eip_4844::{load_trusted_setup_rust, load_trusted_setup_string};
    use rust_kzg_zkcrypto::kzg_types::ZG1;
    use kzg::G1;
    use revm_primitives::kzg::parse_kzg_trusted_setup;
    use lazy_static::lazy_static;

    lazy_static! {
        // "./lib/trusted_setup.txt"
        static ref POINTS: (Box<G1Points>, Box<G2Points>) =  std::fs::read_to_string("trusted_setup.txt")
            .map(|s| parse_kzg_trusted_setup(&s).expect("failed to parse kzg trusted setup"))
            .expect("failed to kzg_parsed_trust_setup.bin");
    }

    #[test]
    fn test_parse_kzg_trusted_setup() {
        
        println!("g1: {:?}", POINTS.0.len());
        println!("g2: {:?}", POINTS.1.len());

        assert_eq!(POINTS.0.len(), MAINNET_KZG_TRUSTED_SETUP.as_ref().secret_g1.len());
        assert_eq!(POINTS.1.len(), MAINNET_KZG_TRUSTED_SETUP.as_ref().secret_g2.len());
    }

    #[test]
    fn test_blob_to_kzg_commitment() {
        let kzg_settings: TaikoKzgSettings = load_trusted_setup_rust(
            G1Points::as_ref(&POINTS.0).flatten(),
            G2Points::as_ref(&POINTS.1).flatten()
        ).unwrap();
        let blob = Blob::from_bytes(&[0u8; 131072]).unwrap();
        let commitment = blob_to_kzg_commitment_rust(
            &deserialize_blob_rust(&blob).unwrap(), 
            &kzg_settings
        )
            .map(|c| c.to_bytes())
            .unwrap();
        assert_eq!(
            commitment_to_version_hash(&commitment).to_string(),
            "0x010657f37554c781402a22917dee2f75def7ab966d7b770905398eba3c444014"
        );
    }

    // #[test]
    // fn test_c_kzg_lib_commitment() {
    //     // check c-kzg mainnet trusted setup is ok
    //     let kzg_settings = Arc::clone(&*MAINNET_KZG_TRUSTED_SETUP);
    //     let blob = [0u8; 131072].into();
    //     let kzg_commit = KzgCommitment::blob_to_kzg_commitment(&blob, &kzg_settings).unwrap();
    //     assert_eq!(
    //         kzg_to_versioned_hash(&kzg_commit).to_string(),
    //         "0x010657f37554c781402a22917dee2f75def7ab966d7b770905398eba3c444014"
    //     );
    // }
}