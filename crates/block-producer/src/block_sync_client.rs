//! L1 and P2P block sync.

use std::{sync::Arc, time::Duration};

use anyhow::{ensure, Context, Result};
use bytes::Bytes;
use ckb_types::prelude::{Builder, Entity, Reader};
use futures::TryStreamExt;
use gw_chain::chain::Chain;
use gw_mem_pool::pool::MemPool;
use gw_p2p_network::{FnSpawn, P2P_BLOCK_SYNC_PROTOCOL, P2P_BLOCK_SYNC_PROTOCOL_NAME};
use gw_rpc_client::rpc_client::RPCClient;
use gw_store::{traits::chain_store::ChainStore, Store};
use gw_types::{
    packed::{
        BlockSync, BlockSyncReader, BlockSyncUnion, P2PBlockSyncResponseReader,
        P2PBlockSyncResponseUnionReader, P2PSyncRequest, Script,
    },
    prelude::Unpack,
};
use tentacle::{
    builder::MetaBuilder,
    service::{ProtocolMeta, ServiceAsyncControl},
    SessionId, SubstreamReadPart,
};
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    Mutex,
};

use crate::{
    chain_updater::ChainUpdater,
    sync_l1::{revert, sync_l1, SyncL1Context},
};

pub struct BlockSyncClient {
    pub store: Store,
    pub rpc_client: RPCClient,
    pub chain: Arc<Mutex<Chain>>,
    pub mem_pool: Arc<Mutex<MemPool>>,
    pub chain_updater: ChainUpdater,
    pub rollup_type_script: Script,
    pub p2p_stream_receiver: UnboundedReceiver<P2PStream>,
    pub completed_initial_syncing: bool,
}

impl SyncL1Context for BlockSyncClient {
    fn store(&self) -> &Store {
        &self.store
    }
    fn rpc_client(&self) -> &RPCClient {
        &self.rpc_client
    }
    fn chain(&self) -> &Mutex<Chain> {
        &self.chain
    }
    fn chain_updater(&self) -> &ChainUpdater {
        &self.chain_updater
    }
    fn rollup_type_script(&self) -> &Script {
        &self.rollup_type_script
    }
}

impl BlockSyncClient {
    pub async fn run(self) {
        let mut client = self;
        let mut p2p_stream = None;
        loop {
            if let Some(ref mut s) = p2p_stream {
                if let Err(err) = run_with_p2p_stream(&mut client, s).await {
                    if err.is::<StreamError>() {
                        // XXX: disconnect this p2p session?
                        p2p_stream = None;
                    }
                    log::warn!("{:#}", err);
                }
                // TODO: backoff.
                tokio::time::sleep(Duration::from_secs(3)).await;
            } else {
                if let Ok(stream) = client.p2p_stream_receiver.try_recv() {
                    p2p_stream = Some(stream);
                    continue;
                }

                if let Err(err) = sync_l1(&client).await {
                    log::warn!("{:#}", err);
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

#[derive(Debug)]
struct StreamError;

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stream error")
    }
}

async fn run_with_p2p_stream(client: &mut BlockSyncClient, stream: &mut P2PStream) -> Result<()> {
    loop {
        sync_l1(client).await?;
        if !client.completed_initial_syncing {
            let mut mem_pool = client.mem_pool.lock().await;
            // XXX: local cells manager.
            let new_tip = client.store.get_last_valid_tip_block_hash()?;
            mem_pool
                .notify_new_tip(new_tip, &Default::default())
                .await?;
            mem_pool.mem_pool_state().set_completed_initial_syncing();
            client.completed_initial_syncing = true;
        }
        let last_confirmed = client
            .store
            .get_last_confirmed_block_number_hash()
            .context("last confirmed")?;
        log::info!("request syncing from {}", last_confirmed.number().unpack());
        let request = P2PSyncRequest::new_builder()
            .block_hash(last_confirmed.block_hash())
            .block_number(last_confirmed.number())
            .build();
        stream.send(request.as_bytes()).await.context(StreamError)?;
        let response = stream
            .recv()
            .await
            .context(StreamError)?
            .context("unexpected end of stream")
            .context(StreamError)?;
        let response = P2PBlockSyncResponseReader::from_slice(&response).context(StreamError)?;
        match response.to_enum() {
            P2PBlockSyncResponseUnionReader::Found(_) => break,
            P2PBlockSyncResponseUnionReader::TryAgain(_) => {}
        }
        log::info!("will try again");
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    log::info!("receiving block sync messages from peer");
    while let Some(msg) = stream.recv().await.context(StreamError)? {
        BlockSyncReader::from_slice(msg.as_ref()).context(StreamError)?;
        let msg = BlockSync::new_unchecked(msg);
        apply_msg(client, msg).await?;
    }
    log::info!("end receiving block sync messages from peer");

    Ok(())
}

async fn apply_msg(client: &BlockSyncClient, msg: BlockSync) -> Result<()> {
    match msg.to_enum() {
        BlockSyncUnion::Revert(r) => {
            // TODO: check block hash.
            log::info!(
                "received revert block {}",
                r.number_hash().number().unpack()
            );
            let store_tx = client.store.begin_transaction();
            revert(client, &store_tx, r.number_hash().number().unpack()).await?;
            store_tx.commit()?;
            let mut mem_pool = client.mem_pool.lock().await;
            mem_pool
                .notify_new_tip(r.number_hash().block_hash().unpack(), &Default::default())
                .await?;
        }
        BlockSyncUnion::LocalBlock(l) => {
            let block_hash = l.block().hash();
            let block_number = l.block().raw().number().unpack();
            log::info!(
                "received block {block_number} {}",
                ckb_types::H256::from(block_hash),
            );
            let store_tx = client.store.begin_transaction();
            let store_block_hash = store_tx.get_block_hash_by_number(block_number)?;
            match store_block_hash {
                None => {
                    // TODO: check parent.
                    let mut chain = client.chain.lock().await;
                    chain
                        .update_local(
                            &store_tx,
                            l.block(),
                            l.deposit_info_vec(),
                            l.deposit_asset_scripts().into_iter().collect(),
                            l.withdrawals().into_iter().collect(),
                            l.post_global_state(),
                        )
                        .await?;
                    chain.calculate_and_store_finalized_custodians(&store_tx, block_number)?;
                    store_tx.commit()?;
                    let mut mem_pool = client.mem_pool.lock().await;
                    mem_pool
                        .notify_new_tip(block_hash.into(), &Default::default())
                        .await?;
                }
                Some(store_block_hash) => {
                    // TODO: revert?
                    ensure!(store_block_hash == block_hash.into());
                }
            }
        }
        BlockSyncUnion::Submitted(s) => {
            // TODO: check block hash.
            log::info!(
                "received submitted block {}",
                s.number_hash().number().unpack()
            );
            let store_tx = client.store.begin_transaction();
            store_tx.set_block_submit_tx_hash(
                s.number_hash().number().unpack(),
                &s.tx_hash().unpack(),
            )?;
            store_tx.set_last_submitted_block_number_hash(&s.number_hash().as_reader())?;
            store_tx.commit()?;
        }
        BlockSyncUnion::Confirmed(c) => {
            // TODO: check block hash.
            log::info!(
                "received confirmed block {}",
                c.number_hash().number().unpack()
            );
            let store_tx = client.store.begin_transaction();
            store_tx.set_last_confirmed_block_number_hash(&c.number_hash().as_reader())?;
            store_tx.commit()?;
        }
    }
    Ok(())
}

pub struct P2PStream {
    id: SessionId,
    control: ServiceAsyncControl,
    read_part: SubstreamReadPart,
}

impl P2PStream {
    async fn recv(&mut self) -> Result<Option<Bytes>> {
        let x = self.read_part.try_next().await?;
        Ok(x)
    }

    async fn send(&mut self, msg: Bytes) -> Result<()> {
        self.control
            .send_message_to(self.id, P2P_BLOCK_SYNC_PROTOCOL, msg)
            .await?;
        Ok(())
    }
}

// XXX: would unbounded channel leak memory?
/// The p2p protocol just sends the p2p stream to the client.
pub fn block_sync_client_protocol(stream_tx: UnboundedSender<P2PStream>) -> ProtocolMeta {
    let spawn = FnSpawn(move |context, control, read_part| {
        let control = control.clone();
        let id = context.id;
        let stream = P2PStream {
            id,
            control,
            read_part,
        };
        let _ = stream_tx.send(stream);
    });
    MetaBuilder::new()
        .name(|_| P2P_BLOCK_SYNC_PROTOCOL_NAME.into())
        .id(P2P_BLOCK_SYNC_PROTOCOL)
        .protocol_spawn(spawn)
        .build()
}
