use std::convert::TryInto;

use ckb_vm::Bytes;
use gw_common::builtins::CKB_SUDT_ACCOUNT_ID;
use gw_types::{
    core::AllowedContractType,
    packed::{ETHAddrRegArgsReader, L2Transaction, MetaContractArgsReader, SUDTArgsReader},
    prelude::*,
};

use crate::error::{AccountError, TransactionValidateError};

/// Types Transaction
pub enum TypedTransaction {
    EthAddrReg(EthAddrRegTx),
    Meta(MetaTx),
    SimpleUDT(SimpleUDTTx),
    Polyjuice(PolyjuiceTx),
}

impl TypedTransaction {
    pub fn from_tx(
        tx: L2Transaction,
        type_: AllowedContractType,
    ) -> Result<Self, TransactionValidateError> {
        let tx = match type_ {
            AllowedContractType::EthAddrReg => Self::EthAddrReg(EthAddrRegTx(tx)),
            AllowedContractType::Meta => Self::Meta(MetaTx(tx)),
            AllowedContractType::Sudt => Self::SimpleUDT(SimpleUDTTx(tx)),
            AllowedContractType::Polyjuice => Self::Polyjuice(PolyjuiceTx(tx)),
            AllowedContractType::Unknown => return Err(AccountError::UnknownScript.into()),
        };
        Ok(tx)
    }

    /// Got expect cost of the tx, (transfer value + fee).
    /// returns none if tx has no cost, it may happend when we call readonly interface of some Godwoken builtin contracts.
    pub fn cost(&self) -> Option<u64> {
        match self {
            Self::EthAddrReg(tx) => tx.cost(),
            Self::Meta(tx) => tx.cost(),
            Self::SimpleUDT(tx) => tx.cost(),
            Self::Polyjuice(tx) => tx.cost(),
        }
    }
}

pub struct EthAddrRegTx(L2Transaction);

impl EthAddrRegTx {
    pub fn cost(&self) -> Option<u64> {
        use gw_types::packed::ETHAddrRegArgsUnionReader::*;

        let args: Bytes = self.0.raw().args().unpack();
        let args = ETHAddrRegArgsReader::from_slice(&args).ok()?;

        match args.to_enum() {
            EthToGw(_) | GwToEth(_) => None,
            SetMapping(args) => Some(args.fee().amount().unpack()),
            BatchSetMapping(args) => Some(args.fee().amount().unpack()),
        }
    }
}

pub struct MetaTx(L2Transaction);

impl MetaTx {
    pub fn cost(&self) -> Option<u64> {
        use gw_types::packed::MetaContractArgsUnionReader::*;

        let args: Bytes = self.0.raw().args().unpack();
        let args = MetaContractArgsReader::from_slice(&args).ok()?;

        match args.to_enum() {
            CreateAccount(args) => Some(args.fee().amount().unpack()),
        }
    }
}

pub struct SimpleUDTTx(L2Transaction);

impl SimpleUDTTx {
    pub fn cost(&self) -> Option<u64> {
        use gw_types::packed::SUDTArgsUnionReader::*;

        let args: Bytes = self.0.raw().args().unpack();
        let args = SUDTArgsReader::from_slice(&args).ok()?;

        match args.to_enum() {
            SUDTQuery(_) => None,
            SUDTTransfer(args) => {
                let fee = args.fee().amount().unpack();
                let to_id: u32 = self.0.raw().to_id().unpack();
                if to_id == CKB_SUDT_ACCOUNT_ID {
                    // CKB transfer cost: transfer CKB value + fee
                    let value = args.amount().unpack();
                    let cost = value.checked_add(fee.into())?;
                    cost.try_into().ok()
                } else {
                    // Simple UDT transfer cost: fee
                    Some(fee)
                }
            }
        }
    }
}

pub struct PolyjuiceTx(L2Transaction);
impl PolyjuiceTx {
    pub fn cost(&self) -> Option<u64> {
        let args: Bytes = self.0.raw().args().unpack();
        if args.len() < 52 {
            log::error!(
                "[gw-generator] parse PolyjuiceTx error, wrong args.len expected: >= 52, actual: {}",
                args.len()
            );
            return None;
        }
        if args[0..7] != b"\xFF\xFF\xFFPOLY"[..] {
            log::error!("[gw-generator] parse PolyjuiceTx error, invalid args",);
            return None;
        }

        // parse gas price, gas limit, value
        let gas_price = {
            let mut data = [0u8; 16];
            data.copy_from_slice(&args[16..32]);
            u128::from_le_bytes(data)
        };
        let gas_limit = {
            let mut data = [0u8; 8];
            data.copy_from_slice(&args[8..16]);
            u64::from_le_bytes(data)
        };

        let value = {
            let mut data = [0u8; 16];
            data.copy_from_slice(&args[32..48]);
            u128::from_le_bytes(data)
        };
        let cost = value.checked_add(gas_price.checked_mul(gas_limit.into())?)?;
        cost.try_into().ok()
    }
}
