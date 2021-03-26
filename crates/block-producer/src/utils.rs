use std::convert::TryInto;

use crate::{rpc_client::RPCClient, transaction_skeleton::TransactionSkeleton};
use anyhow::{anyhow, Result};
use gw_block_producer::types::InputCellInfo;
use gw_types::{
    packed::{CellInput, Script},
    prelude::*,
};

/// Calculate tx fee
fn calculate_required_tx_fee(tx_size: usize) -> u64 {
    // tx_size * KB / MIN_FEE_RATE
    tx_size as u64
}

/// calculate tx skeleton inputs / outputs
fn calculate_paid_fee(tx_skeleton: &TransactionSkeleton) -> Result<(u128, u128)> {
    let mut input_capacity: u128 = 0;
    for input in tx_skeleton.inputs() {
        let capacity: u64 = input.cell.output.capacity().unpack();
        input_capacity = input_capacity
            .checked_add(capacity.into())
            .ok_or(anyhow!("overflow"))?;
    }

    let mut output_capacity: u128 = 0;
    for (output, _data) in tx_skeleton.outputs() {
        let capacity: u64 = output.capacity().unpack();
        output_capacity = output_capacity
            .checked_add(capacity.into())
            .ok_or(anyhow!("overflow"))?;
    }
    Ok((input_capacity, output_capacity))
}

/// Add fee cell to tx skeleton
pub async fn fill_tx_fee(
    tx_skeleton: &mut TransactionSkeleton,
    rpc_client: &RPCClient,
    lock_script: Script,
) -> Result<()> {
    let tx_size: usize = tx_skeleton.tx_in_block_size()?;
    let (input_capacity, output_capacity) = calculate_paid_fee(tx_skeleton)?;
    assert!(
        input_capacity >= output_capacity,
        "Rollup cells capacity should be enough to use"
    );
    let paid_fee: u64 = (input_capacity - output_capacity)
        .try_into()
        .expect("paid fee too large");
    // calculate required fee
    let required_fee = calculate_required_tx_fee(tx_size)
        .checked_sub(paid_fee)
        .unwrap_or(0);

    // find a cell to pay tx fee
    if required_fee > 0 {
        // get payment cells
        let cells = rpc_client
            .query_payment_cells(lock_script, required_fee)
            .await?;
        // put cells in tx skeleton
        tx_skeleton
            .inputs_mut()
            .extend(cells.into_iter().map(|cell| {
                let input = CellInput::new_builder()
                    .previous_output(cell.out_point.clone())
                    .build();
                InputCellInfo { input, cell }
            }));
    }
    Ok(())
}
