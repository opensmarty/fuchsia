// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Error,
    fidl::endpoints::ServerEnd,
    fidl::Channel,
    fidl_fidl_examples_routing_echo as fecho, fuchsia_async as fasync,
    futures::{channel::*, lock::Mutex, sink::SinkExt, StreamExt, TryStreamExt},
    std::sync::Arc,
};

#[must_use = "invoke resume() otherwise the client will be halted indefinitely!"]
pub struct Echo {
    pub message: String,
    // This Sender is used to unblock the client that sent the echo.
    responder: oneshot::Sender<()>,
}

impl Echo {
    pub fn resume(self) {
        self.responder.send(()).unwrap()
    }
}

#[derive(Clone)]
pub struct EchoSender {
    tx: Arc<Mutex<mpsc::Sender<Echo>>>,
}

impl EchoSender {
    fn new(tx: mpsc::Sender<Echo>) -> Self {
        Self { tx: Arc::new(Mutex::new(tx)) }
    }

    /// Sends the event to a receiver. Returns a responder which can be blocked on.
    async fn send(&self, message: String) -> Result<oneshot::Receiver<()>, Error> {
        let (responder_tx, responder_rx) = oneshot::channel();
        {
            let mut tx = self.tx.lock().await;
            tx.send(Echo { message, responder: responder_tx }).await?;
        }
        Ok(responder_rx)
    }
}

pub struct EchoReceiver {
    rx: mpsc::Receiver<Echo>,
}

impl EchoReceiver {
    fn new(rx: mpsc::Receiver<Echo>) -> Self {
        Self { rx }
    }

    /// Receives the next invocation from the sender.
    pub async fn next(&mut self) -> Option<Echo> {
        self.rx.next().await
    }
}

/// Capability that serves the Echo FIDL protocol in one task and allows
/// another task to wait on a echo arriving via a EchoReceiver.
#[derive(Clone)]
pub struct EchoCapability {
    tx: EchoSender,
}

impl EchoCapability {
    pub fn new() -> (Self, EchoReceiver) {
        let (tx, rx) = mpsc::channel(0);
        let sender = EchoSender::new(tx);
        let receiver = EchoReceiver::new(rx);
        (Self { tx: sender }, receiver)
    }

    pub fn serve(&self, mut stream: fecho::EchoRequestStream) {
        let sender = self.tx.clone();
        fasync::spawn(async move {
            while let Some(event) = stream.try_next().await.expect("failed to serve echo service") {
                let fecho::EchoRequest::EchoString { value, responder } = event;
                let echo = sender
                    .send(value.clone().unwrap_or(String::new()))
                    .await
                    .expect("failed to send echo to test");
                echo.await.expect("Failed to receive a response");
                responder.send(value.as_deref()).expect("failed to send echo response");
            }
        });
    }

    pub fn serve_async(&self) -> Box<dyn Fn(Channel) + Send> {
        let this = self.clone();
        Box::new(move |channel| {
            let stream = ServerEnd::<fecho::EchoMarker>::new(channel)
                .into_stream()
                .expect("could not convert channel into stream");
            let that = this.clone();
            that.serve(stream);
        })
    }
}