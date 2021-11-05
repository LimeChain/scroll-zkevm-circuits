use super::super::utils;
use super::super::CaseAllocation;
use super::super::Cell;
use crate::evm_circuit::param::MAX_BYTES_FIELD;
use crate::util::Expr;
use array_init::array_init;
use halo2::plonk::Error;
use halo2::{arithmetic::FieldExt, circuit::Region, plonk::Expression};

use super::constraint_builder::ConstraintBuilder;

// Returns `1` when `value == 0`, and returns `0` otherwise.
#[derive(Clone, Debug)]
pub struct IsZeroGadget<F> {
    pub(crate) inverse: Cell<F>,
}

impl<F: FieldExt> IsZeroGadget<F> {
    pub const NUM_CELLS: usize = 1;
    pub const NUM_WORDS: usize = 0;

    pub(crate) fn construct(alloc: &mut CaseAllocation<F>) -> Self {
        Self {
            inverse: alloc.cells.pop().unwrap(),
        }
    }

    pub(crate) fn constraints(
        &self,
        cb: &mut ConstraintBuilder<F>,
        value: Expression<F>,
    ) -> Expression<F> {
        let is_zero = 1.expr() - (value.clone() * self.inverse.expr());
        // when `value != 0` check `inverse = a.invert()`: value * (1 - value * inverse)
        cb.add_expression(value * is_zero.clone());
        // when `value == 0` check `inverse = 0`: `inverse ⋅ (1 - value * inverse)`
        cb.add_expression(self.inverse.expr() * is_zero.clone());

        is_zero
    }

    pub(crate) fn assign(
        &self,
        region: &mut Region<'_, F>,
        offset: usize,
        value: F,
    ) -> Result<F, Error> {
        let inverse = value.invert().unwrap_or(F::zero());
        self.inverse.assign(region, offset, Some(inverse))?;
        Ok(if value.is_zero() { F::one() } else { F::zero() })
    }
}

// Returns `1` when `lhs == rhs`, and returns `0` otherwise.
#[derive(Clone, Debug)]
pub struct IsEqualGadget<F> {
    pub(crate) is_zero: IsZeroGadget<F>,
}

impl<F: FieldExt> IsEqualGadget<F> {
    pub const NUM_CELLS: usize = IsZeroGadget::<F>::NUM_CELLS;
    pub const NUM_WORDS: usize = IsZeroGadget::<F>::NUM_WORDS;

    pub(crate) fn construct(alloc: &mut CaseAllocation<F>) -> Self {
        Self {
            is_zero: IsZeroGadget::<F>::construct(alloc),
        }
    }

    pub(crate) fn constraints(
        &self,
        cb: &mut ConstraintBuilder<F>,
        lhs: Expression<F>,
        rhs: Expression<F>,
    ) -> Expression<F> {
        self.is_zero.constraints(cb, (lhs) - (rhs))
    }

    pub(crate) fn assign(
        &self,
        region: &mut Region<'_, F>,
        offset: usize,
        lhs: F,
        rhs: F,
    ) -> Result<F, Error> {
        self.is_zero.assign(region, offset, lhs - rhs)
    }
}

/// Returns `1` when `lhs < rhs`, and returns `0` otherwise.
/// lhs and rhs `< 256**NUM_BYTES`
/// `NUM_BYTES` is required to be `<= MAX_BYTES_FIELD` to prevent overflow:
/// values are stored in a single field element and two of these
/// are added together.
/// The equation that is enforced is `lhs - rhs == diff - (lt * range)`.
/// Because all values are `<= 256**NUM_BYTES` and `lt` is boolean,
///  `lt` can only be `1` when `lhs < rhs`.
#[derive(Clone, Debug)]
pub struct LtGadget<F, const NUM_BYTES: usize> {
    lt: Cell<F>, // `1` when `lhs < rhs`, `0` otherwise.
    diff: [Cell<F>; NUM_BYTES], /* The byte values of `diff`.
                  * `diff` equals `lhs - rhs` if `lhs >= rhs`,
                  * `lhs - rhs + range` otherwise. */
    range: F, // The range of the inputs, `256**NUM_BYTES`
}

impl<F: FieldExt, const NUM_BYTES: usize> LtGadget<F, NUM_BYTES> {
    pub const NUM_CELLS: usize = 1 + NUM_BYTES; // lt + diff
    pub const NUM_WORDS: usize = 0;

    pub(crate) fn construct(alloc: &mut CaseAllocation<F>) -> Self {
        assert!(NUM_BYTES <= MAX_BYTES_FIELD, "unsupported number of bytes");
        Self {
            lt: alloc.cells.pop().unwrap(),
            diff: array_init(|_| alloc.cells.pop().unwrap()),
            range: utils::get_range(NUM_BYTES * 8),
        }
    }

    pub(crate) fn constraints(
        &self,
        cb: &mut ConstraintBuilder<F>,
        lhs: Expression<F>,
        rhs: Expression<F>,
    ) -> Expression<F> {
        let diff = utils::from_bytes::expr(self.diff.to_vec());
        // The equation we require to hold: `lhs - rhs == diff - (lt * range)`.
        cb.require_equal(lhs - rhs, diff - (self.lt.expr() * self.range));

        // `lt` needs to be boolean
        cb.require_boolean(self.lt.expr());
        // All parts of `diff` need to be bytes
        for byte in self.diff.iter() {
            cb.require_in_range(byte.expr(), 256);
        }

        self.lt.expr()
    }

    pub(crate) fn assign(
        &self,
        region: &mut Region<'_, F>,
        offset: usize,
        lhs: F,
        rhs: F,
    ) -> Result<(F, Vec<u8>), Error> {
        // Set `lt`
        let lt = lhs < rhs;
        self.lt.assign(
            region,
            offset,
            Some(F::from_u64(if lt { 1 } else { 0 })),
        )?;

        // Set the bytes of diff
        let diff = (lhs - rhs) + (if lt { self.range } else { F::zero() });
        let diff_bytes = diff.to_bytes();
        for (idx, diff) in self.diff.iter().enumerate() {
            diff.assign(
                region,
                offset,
                Some(F::from_u64(diff_bytes[idx] as u64)),
            )?;
        }

        Ok((if lt { F::one() } else { F::zero() }, diff_bytes.to_vec()))
    }

    pub(crate) fn diff_bytes(&self) -> Vec<Cell<F>> {
        self.diff.to_vec()
    }
}

/// Returns (lt, eq):
/// - `lt` is `1` when `lhs < rhs`, `0` otherwise.
/// - `eq` is `1` when `lhs == rhs`, `0` otherwise.
/// lhs and rhs `< 256**NUM_BYTES`
/// `NUM_BYTES` is required to be `<= MAX_BYTES_FIELD`.
#[derive(Clone, Debug)]
pub struct ComparisonGadget<F, const NUM_BYTES: usize> {
    lt: LtGadget<F, NUM_BYTES>,
    eq: IsZeroGadget<F>,
}

impl<F: FieldExt, const NUM_BYTES: usize> ComparisonGadget<F, NUM_BYTES> {
    pub const NUM_CELLS: usize =
        LtGadget::<F, NUM_BYTES>::NUM_CELLS + IsZeroGadget::<F>::NUM_CELLS;
    pub const NUM_WORDS: usize = 0;

    pub(crate) fn construct(alloc: &mut CaseAllocation<F>) -> Self {
        Self {
            lt: LtGadget::<F, NUM_BYTES>::construct(alloc),
            eq: IsZeroGadget::<F>::construct(alloc),
        }
    }

    pub(crate) fn constraints(
        &self,
        cb: &mut ConstraintBuilder<F>,
        lhs: Expression<F>,
        rhs: Expression<F>,
    ) -> (Expression<F>, Expression<F>) {
        // lt
        let lt = self.lt.constraints(cb, lhs, rhs);

        // eq
        let eq = self
            .eq
            .constraints(cb, utils::sum::expr(&self.lt.diff_bytes()[..]));

        (lt, eq)
    }

    pub(crate) fn assign(
        &self,
        region: &mut Region<'_, F>,
        offset: usize,
        lhs: F,
        rhs: F,
    ) -> Result<(F, F), Error> {
        // lt
        let (lt, diff) = self.lt.assign(region, offset, lhs, rhs)?;

        // eq
        let eq =
            self.eq
                .assign(region, offset, utils::sum::value(&diff[..]))?;

        Ok((lt, eq))
    }
}