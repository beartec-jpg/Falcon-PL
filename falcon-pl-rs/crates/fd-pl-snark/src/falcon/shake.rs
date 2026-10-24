//! SHAKE256 (FIPS 202) as an R1CS gadget over BN254.
//!
//! HashToPoint is SHAKE256(nonce || message) then 16-bit rejection.
//! Without this, a Falcon-in-circuit statement would not bind the header.

use ark_bn254::Fr;
use ark_r1cs_std::boolean::Boolean;
use ark_r1cs_std::fields::fp::FpVar;
use ark_r1cs_std::prelude::*;
use ark_relations::r1cs::SynthesisError;

pub const RATE_BYTES: usize = 136; // SHAKE256 rate = 1088 bits
const WIDTH: usize = 1600;
const LANES: usize = 25;
const ROUNDS: usize = 24;

const RC: [u64; ROUNDS] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_8082,
    0x8000_0000_0000_808A,
    0x8000_0000_8000_8000,
    0x0000_0000_0000_808B,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8009,
    0x0000_0000_0000_008A,
    0x0000_0000_0000_0088,
    0x0000_0000_8000_8009,
    0x0000_0000_8000_000A,
    0x0000_0000_8000_808B,
    0x8000_0000_0000_008B,
    0x8000_0000_0000_8089,
    0x8000_0000_0000_8003,
    0x8000_0000_0000_8002,
    0x8000_0000_0000_0080,
    0x0000_0000_0000_800A,
    0x8000_0000_8000_000A,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8080,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8008,
];

/// ρ offsets RHO[x][y] from FIPS 202 Table 2.
const RHO: [[u32; 5]; 5] = [
    [0, 36, 3, 41, 18],
    [1, 44, 10, 45, 2],
    [62, 6, 43, 15, 61],
    [28, 55, 25, 21, 56],
    [27, 20, 39, 8, 14],
];

fn xor(a: &Boolean<Fr>, b: &Boolean<Fr>) -> Result<Boolean<Fr>, SynthesisError> {
    a.xor(b)
}

fn lane_off(x: usize, y: usize) -> usize {
    (x + 5 * y) * 64
}

fn rotl_lane(lane: &[Boolean<Fr>], r: u32) -> Vec<Boolean<Fr>> {
    let r = (r as usize) % 64;
    let mut out = Vec::with_capacity(64);
    for i in 0..64 {
        out.push(lane[(i + 64 - r) % 64].clone());
    }
    out
}

fn xor_lanes(a: &[Boolean<Fr>], b: &[Boolean<Fr>]) -> Result<Vec<Boolean<Fr>>, SynthesisError> {
    a.iter().zip(b).map(|(x, y)| xor(x, y)).collect()
}

fn keccak_f(state: &mut [Boolean<Fr>]) -> Result<(), SynthesisError> {
    debug_assert_eq!(state.len(), WIDTH);
    for round in 0..ROUNDS {
        // θ
        let mut c = Vec::with_capacity(5);
        for x in 0..5 {
            let mut col = state[lane_off(x, 0)..lane_off(x, 0) + 64].to_vec();
            for y in 1..5 {
                col = xor_lanes(&col, &state[lane_off(x, y)..lane_off(x, y) + 64])?;
            }
            c.push(col);
        }
        let mut d = Vec::with_capacity(5);
        for x in 0..5 {
            let rot = rotl_lane(&c[(x + 1) % 5], 1);
            d.push(xor_lanes(&c[(x + 4) % 5], &rot)?);
        }
        for x in 0..5 {
            for y in 0..5 {
                let off = lane_off(x, y);
                let xored = xor_lanes(&state[off..off + 64], &d[x])?;
                state[off..off + 64].clone_from_slice(&xored);
            }
        }

        // ρ then π: B[y][(2x+3y)%5] = rotl(A[x][y], r[x][y])
        let mut b = vec![Boolean::FALSE; WIDTH];
        for x in 0..5 {
            for y in 0..5 {
                let rotated = rotl_lane(&state[lane_off(x, y)..lane_off(x, y) + 64], RHO[x][y]);
                let nx = y;
                let ny = (2 * x + 3 * y) % 5;
                let off = lane_off(nx, ny);
                b[off..off + 64].clone_from_slice(&rotated);
            }
        }

        // χ: A[x] = B[x] xor ((not B[x+1]) and B[x+2])
        for x in 0..5 {
            for y in 0..5 {
                let off = lane_off(x, y);
                let o1 = lane_off((x + 1) % 5, y);
                let o2 = lane_off((x + 2) % 5, y);
                for z in 0..64 {
                    let not1 = b[o1 + z].not();
                    let t = not1.and(&b[o2 + z])?;
                    state[off + z] = xor(&b[off + z], &t)?;
                }
            }
        }

        // ι
        let rc = RC[round];
        for z in 0..64 {
            if (rc >> z) & 1 == 1 {
                state[z] = state[z].not();
            }
        }
    }
    let _ = LANES;
    Ok(())
}

fn xor_const_byte(state: &mut [Boolean<Fr>], byte_i: usize, val: u8) -> Result<(), SynthesisError> {
    for bit in 0..8 {
        if (val >> bit) & 1 == 1 {
            let i = byte_i * 8 + bit;
            state[i] = state[i].not();
        }
    }
    Ok(())
}

fn xor_byte_var(
    state: &mut [Boolean<Fr>],
    byte_i: usize,
    bits: &[Boolean<Fr>],
) -> Result<(), SynthesisError> {
    for bit in 0..8 {
        let i = byte_i * 8 + bit;
        state[i] = xor(&state[i], &bits[bit])?;
    }
    Ok(())
}

fn byte_from_state(state: &[Boolean<Fr>], byte_i: usize) -> Result<FpVar<Fr>, SynthesisError> {
    Boolean::le_bits_to_fp_var(&state[byte_i * 8..byte_i * 8 + 8])
}

/// SHAKE256 absorb `input` (8-bit FpVars) and squeeze `out_len` bytes.
pub fn shake256_var(
    input: &[FpVar<Fr>],
    out_len: usize,
) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    let mut state = vec![Boolean::<Fr>::FALSE; WIDTH];
    let mut offset = 0usize;
    for b in input {
        let bits = crate::falcon::gadget::enforce_num_bits(b, 8)?;
        xor_byte_var(&mut state, offset, &bits)?;
        offset += 1;
        if offset == RATE_BYTES {
            keccak_f(&mut state)?;
            offset = 0;
        }
    }
    xor_const_byte(&mut state, offset, 0x1F)?;
    xor_const_byte(&mut state, RATE_BYTES - 1, 0x80)?;
    keccak_f(&mut state)?;

    let mut out = Vec::with_capacity(out_len);
    loop {
        for i in 0..RATE_BYTES {
            if out.len() == out_len {
                return Ok(out);
            }
            out.push(byte_from_state(&state, i)?);
        }
        keccak_f(&mut state)?;
    }
}

pub fn bytes_from_u8(cs: ark_relations::r1cs::ConstraintSystemRef<Fr>, data: &[u8]) -> Result<Vec<FpVar<Fr>>, SynthesisError> {
    data.iter()
        .map(|b| FpVar::new_witness(cs.clone(), || Ok(Fr::from(*b as u64))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_relations::r1cs::ConstraintSystem;
    use sha3::digest::{ExtendableOutput, Update, XofReader};
    use sha3::Shake256;

    fn native_shake(input: &[u8], n: usize) -> Vec<u8> {
        let mut h = Shake256::default();
        h.update(input);
        let mut r = h.finalize_xof();
        let mut out = vec![0u8; n];
        r.read(&mut out);
        out
    }

    fn fp_to_u8(x: &FpVar<Fr>) -> u8 {
        use ark_ff::{BigInteger, PrimeField};
        use ark_r1cs_std::R1CSVar;
        let f = x.value().unwrap();
        let b = f.into_bigint().to_bytes_le();
        b[0]
    }

    #[test]
    fn shake256_gadget_matches_fips() {
        let cs = ConstraintSystem::<Fr>::new_ref();
        let msg = b"abc";
        let ins = bytes_from_u8(cs.clone(), msg).unwrap();
        for b in &ins {
            crate::falcon::gadget::enforce_num_bits(b, 8).unwrap();
        }
        let out = shake256_var(&ins, 32).unwrap();
        assert!(cs.is_satisfied().unwrap(), "shake r1cs");
        let got: Vec<u8> = out.iter().map(fp_to_u8).collect();
        let want = native_shake(msg, 32);
        assert_eq!(got, want, "SHAKE256 gadget must match sha3 crate / FIPS 202");
        eprintln!(
            "shake256('abc') constraints={} witnesses={}",
            cs.num_constraints(),
            cs.num_witness_variables()
        );
    }
}
