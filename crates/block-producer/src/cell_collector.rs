use anyhow::Result;
use gw_block_producer::types::CellInfo;
use gw_types::packed::{DepositionRequest, OutPoint, Transaction};

pub struct DepositInfo {
    pub request: DepositionRequest,
    pub cell: CellInfo,
}

pub struct CellCollector;

impl CellCollector {
    /// return all lived deposition requests
    pub fn query_deposit_cells(&self) -> Vec<DepositInfo> {
        unimplemented!()
    }

    /// query lived rollup cell
    pub fn query_rollup_cell(&self) -> Option<CellInfo> {
        unimplemented!()
    }

    pub fn send_transaction(&self, tx: Transaction) -> Result<()> {
        unimplemented!()
    }

    pub fn get_cell(&self, out_point: &OutPoint) -> Option<CellInfo> {
        unimplemented!()
    }

    /// query payment cells, the returned cells should provide at least total_capacity fee,
    /// and the remained fees should be enough to cover a charge cell
    pub fn query_payment_cells(&self, lock_hash: &[u8; 32], total_capacity: u64) -> Vec<CellInfo> {
        unimplemented!()
    }
}
