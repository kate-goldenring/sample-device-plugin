pub(crate) mod device_plugin_api {
    pub(crate) mod v1beta1 {
        pub const API_VERSION: &str = "v1beta1";
        tonic::include_proto!("v1beta1");
    }
}
mod device_plugin;

#[cfg(target_family = "unix")]
const DEFAULT_PLUGIN_PATH: &str = "/var/lib/kubelet/device-plugins/";
const PLUGIN_NAME: &str = "mock_plugin";

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    println!("Starting device plugin");
    let args: Vec<String> = std::env::args().collect();
    let dp_dir: &str = {
        if args.len() > 1 {
            println!("path is {}", &args[0]);
            &args[1]
        } else {
            DEFAULT_PLUGIN_PATH
        }
    };
    let name = {
        if args.len() > 2 {
            println!("name is {}", &args[1]);
            &args[2]
        } else {
            PLUGIN_NAME
        }
    };
    let socket_name = format!("{}.sock", name);
    let dp_socket = format!("{}{}", dp_dir, socket_name);
    println!("running device plugin on socket {}", dp_socket);
    let kubelet_socket = format!("{}{}.sock", dp_dir, "kubelet");
    let resource_name = format!("example.com/{}", name);
    let res = run(&dp_socket, &socket_name, &resource_name, &kubelet_socket).await;
    println!("about to remove socket");
    std::fs::remove_file(dp_socket).unwrap_or(());
    println!("removed socket");
    res?;
    Ok(())
}

async fn run(
    dp_socket: &str,
    socket_name: &str,
    resource_name: &str,
    kubelet_socket: &str,
) -> anyhow::Result<()> {
    let (_tx, rx) = tokio::sync::watch::channel(device_plugin::make_devices());
    let thread_socket_path = dp_socket.to_string();
    let device_plugin_thread = tokio::spawn(async move {
        device_plugin::run_mock_device_plugin(
            rx,
            DEFAULT_PLUGIN_PATH.to_string(),
            thread_socket_path.clone(),
        )
        .await
        .unwrap()
    });
    // wait for DP to be served
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    device_plugin::register_mock_device_plugin(&kubelet_socket, &socket_name, &resource_name)
        .await?;

    device_plugin_thread.await?;
    Ok(())
}
