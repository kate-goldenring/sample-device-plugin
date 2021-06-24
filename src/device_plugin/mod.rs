use crate::device_plugin_api::v1beta1::{
    device_plugin_server::{DevicePlugin, DevicePluginServer},
    registration_client, AllocateRequest, AllocateResponse, ContainerAllocateResponse, Device,
    DevicePluginOptions, Empty, ListAndWatchResponse, PreStartContainerRequest,
    PreStartContainerResponse, PreferredAllocationRequest, PreferredAllocationResponse,
    RegisterRequest, API_VERSION,
};
use futures::Stream;
use futures::TryFutureExt;
use std::convert::TryFrom;
use std::pin::Pin;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tonic::transport::{Endpoint, Server, Uri};
use tonic::{Request, Response, Status};

const HEALTHY: &str = "Healthy";
const UNHEALTHY: &str = "Unhealthy";

/// Mock Device Plugin for testing the DeviceManager
/// Sends a new list of devices to the DeviceManager whenever it's `devices_receiver`
/// is notified of them on a channel.
pub struct MockDevicePlugin {
    // Using watch so the receiver can be cloned and be moved into a spawned thread in ListAndWatch
    devices_receiver: tokio::sync::watch::Receiver<Vec<Device>>,
}

#[async_trait::async_trait]
impl DevicePlugin for MockDevicePlugin {
    async fn get_device_plugin_options(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<DevicePluginOptions>, Status> {
        unimplemented!();
    }

    type ListAndWatchStream =
        Pin<Box<dyn Stream<Item = Result<ListAndWatchResponse, Status>> + Send + Sync + 'static>>;
    async fn list_and_watch(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::ListAndWatchStream>, Status> {
        println!("list_and_watch entered");
        // Create a channel that list_and_watch can periodically send updates to kubelet on
        let (kubelet_update_sender, kubelet_update_receiver) = tokio::sync::mpsc::channel(3);
        let mut devices_receiver = self.devices_receiver.clone();
        tokio::spawn(async move {
            // Send first devices
            let devices = devices_receiver.borrow().clone();
            if !devices.is_empty() {
                kubelet_update_sender
                    .send(Ok(ListAndWatchResponse { devices }))
                    .await
                    .unwrap();
            }
            while devices_receiver.changed().await.is_ok() {
                let devices = devices_receiver.borrow().clone();
                println!(
                    "list_and_watch received new devices [{:?}] to send",
                    devices
                );
                kubelet_update_sender
                    .send(Ok(ListAndWatchResponse { devices }))
                    .await
                    .unwrap();
            }
        });
        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(kubelet_update_receiver),
        )))
    }

    async fn get_preferred_allocation(
        &self,
        _request: Request<PreferredAllocationRequest>,
    ) -> Result<Response<PreferredAllocationResponse>, Status> {
        unimplemented!();
    }

    async fn allocate(
        &self,
        request: Request<AllocateRequest>,
    ) -> Result<Response<AllocateResponse>, Status> {
        println!("allocate called");
        let allocate_request = request.into_inner();
        let mut envs = std::collections::HashMap::new();
        envs.insert("MOCK_ENV_VAR".to_string(), "1".to_string());
        let container_responses: Vec<ContainerAllocateResponse> = allocate_request
            .container_requests
            .into_iter()
            .map(|_| ContainerAllocateResponse {
                envs: envs.clone(),
                ..Default::default()
            })
            .collect();
        Ok(Response::new(AllocateResponse {
            container_responses,
        }))
    }

    async fn pre_start_container(
        &self,
        _request: Request<PreStartContainerRequest>,
    ) -> Result<Response<PreStartContainerResponse>, Status> {
        Ok(Response::new(PreStartContainerResponse {}))
    }
}

/// Serves the mock DP and returns its socket path
pub async fn run_mock_device_plugin(
    devices_receiver: watch::Receiver<Vec<Device>>,
    socket_dir: String,
    socket_path: String,
) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(&socket_dir).await?;
    let device_plugin = MockDevicePlugin { devices_receiver };
    println!("Serving device plugin at {}", socket_path);
    let incoming = {
        let uds = UnixListener::bind(socket_path)?;

        async_stream::stream! {
            while let item = uds.accept().map_ok(|(st, _)| unix::UnixStream(st)).await {
                yield item;
            }
        }
    };

    Server::builder()
        .add_service(DevicePluginServer::new(device_plugin))
        .serve_with_incoming(incoming)
        .await?;
    println!("Device plugin service ended");

    Ok(())
}

/// Registers the mock DP with the DeviceManager's registration service
pub async fn register_mock_device_plugin(
    kubelet_socket: &str,
    socket_name: &str,
    dp_resource_name: &str,
) -> anyhow::Result<()> {
    let op = DevicePluginOptions {
        get_preferred_allocation_available: false,
        pre_start_required: false,
    };
    let register_request = tonic::Request::new(RegisterRequest {
        version: API_VERSION.into(),
        endpoint: socket_name.to_string(),
        resource_name: dp_resource_name.to_string(),
        options: Some(op),
    });
    let closure_socket = kubelet_socket.to_string();
    let channel = Endpoint::try_from("dummy://[::]:50051")?
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            UnixStream::connect(closure_socket.to_string())
        }))
        .await?;
    let mut registration_client = registration_client::RegistrationClient::new(channel);
    registration_client.register(register_request).await?;
    Ok(())
}

pub fn make_devices() -> Vec<Device> {
    // Make 3 mock devices
    let d1 = Device {
        id: "d1".to_string(),
        health: HEALTHY.to_string(),
        topology: None,
    };
    let d2 = Device {
        id: "d2".to_string(),
        health: HEALTHY.to_string(),
        topology: None,
    };
    let d3 = Device {
        id: "d3".to_string(),
        health: UNHEALTHY.to_string(),
        topology: None,
    };
    vec![d1, d2, d3]
}

#[cfg(unix)]
mod unix {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tonic::transport::server::Connected;

    #[derive(Debug)]
    pub struct UnixStream(pub tokio::net::UnixStream);

    impl Connected for UnixStream {}

    impl AsyncRead for UnixStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for UnixStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }
}
