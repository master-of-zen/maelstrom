use crate::scheduler_task::{SchedulerMessage, SchedulerSender};
use anyhow::Result;
use meticulous_base::{
    proto::{ArtifactFetcherToBroker, BrokerToArtifactFetcher},
    Sha256Digest,
};
use meticulous_util::{
    fs::{File, Fs},
    net,
};
use slog::{debug, Logger};
use std::{io, net::TcpStream, sync::mpsc};

fn get_file<'fs>(
    fs: &'fs Fs,
    digest: &Sha256Digest,
    scheduler_sender: &SchedulerSender,
) -> Result<(File<'fs>, u64)> {
    let (channel_sender, channel_receiver) = mpsc::channel();
    scheduler_sender.send(SchedulerMessage::GetArtifactForWorker(
        digest.clone(),
        channel_sender,
    ))?;

    let (path, size) = channel_receiver.recv()??;
    let f = fs.open_file(path)?;
    Ok((f, size))
}

fn handle_one_message(
    msg: ArtifactFetcherToBroker,
    mut socket: &mut TcpStream,
    scheduler_sender: &SchedulerSender,
    log: &mut Logger,
) -> Result<()> {
    debug!(log, "received artifact fetcher message"; "msg" => ?msg);
    let ArtifactFetcherToBroker(digest) = msg;
    let fs = Fs::new();
    let result = get_file(&fs, &digest, scheduler_sender);
    let msg = BrokerToArtifactFetcher(
        result
            .as_ref()
            .map(|(_, size)| *size)
            .map_err(|e| e.to_string()),
    );
    debug!(log, "sending artifact fetcher message"; "msg" => ?msg);
    net::write_message_to_socket(&mut socket, msg)?;
    let (mut f, size) = result?;
    let copied = io::copy(&mut f, &mut socket)?;
    assert_eq!(copied, size);
    scheduler_sender.send(SchedulerMessage::DecrementRefcount(digest))?;
    Ok(())
}

fn connection_loop(
    mut socket: TcpStream,
    scheduler_sender: &SchedulerSender,
    log: &mut Logger,
) -> Result<()> {
    loop {
        let msg = net::read_message_from_socket(&mut socket)?;
        handle_one_message(msg, &mut socket, scheduler_sender, log)?;
    }
}

pub fn connection_main(
    socket: TcpStream,
    scheduler_sender: SchedulerSender,
    mut log: Logger,
) -> Result<()> {
    debug!(log, "artifact fetcher connected");
    let err = connection_loop(socket, &scheduler_sender, &mut log).unwrap_err();
    debug!(log, "artifact fetcher disconnected"; "err" => %err);
    Err(err)
}
