use anyhow::{anyhow, Result};
use maelstrom_base::{
    proto::{ArtifactFetcherToBroker, BrokerToArtifactFetcher, Hello},
    ArtifactType, Sha256Digest,
};
use maelstrom_util::{config::BrokerAddr, io::ChunkedReader, net};
use slog::{debug, Logger};
use std::{
    io::{self, BufReader},
    net::TcpStream,
    path::PathBuf,
};
use tar::Archive;

pub fn main(
    digest: &Sha256Digest,
    type_: ArtifactType,
    path: PathBuf,
    broker_addr: BrokerAddr,
    log: &mut Logger,
) -> Result<u64> {
    let mut writer = TcpStream::connect(broker_addr.inner())?;
    let mut reader = BufReader::new(writer.try_clone()?);
    net::write_message_to_socket(&mut writer, Hello::ArtifactFetcher)?;

    let msg = ArtifactFetcherToBroker(digest.clone(), type_);
    debug!(log, "artifact fetcher sending message"; "msg" => ?msg);

    net::write_message_to_socket(&mut writer, msg)?;
    let msg = net::read_message_from_socket::<BrokerToArtifactFetcher>(&mut reader)?;
    debug!(log, "artifact fetcher received message"; "msg" => ?msg);
    msg.0
        .map_err(|e| anyhow!("Broker error reading artifact: {e}"))?;

    let mut reader = countio::Counter::new(ChunkedReader::new(reader));
    Archive::new(&mut reader).unpack(path)?;

    // N.B. Make sure archive wasn't truncated by reading ending chunk.
    io::copy(&mut reader, &mut io::sink())?;

    Ok(reader.reader_bytes() as u64)
}
