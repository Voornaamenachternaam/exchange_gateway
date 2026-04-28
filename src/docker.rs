// src/docker.rs
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bollard::Docker;
use bollard::query_parameters::{ListContainersOptionsBuilder, InspectContainerOptionsBuilder};
use docker_credential::{DockerCredential, get_credential};
use dockworker::{Docker as DockworkerDocker, ContainerCreateOptions};

/// Check whether the Docker daemon is reachable via bollard.
pub async fn check_docker_daemon_available() -> bool {
    match Docker::connect_with_socket_defaults() {
        Ok(docker) => docker.ping().await.is_ok(),
        Err(_) => false,
    }
}

/// Retrieve Docker registry credentials for the given server using docker_credential.
pub fn get_docker_registry_credential(server: &str) -> Option<String> {
    match get_credential(server) {
        Ok(DockerCredential::UsernamePassword(username, password)) => {
            let encoded = STANDARD.encode(format!("{}:{}", username, password));
            Some(encoded)
        }
        Ok(DockerCredential::IdentityToken(token)) => Some(token),
        Err(_) => None,
    }
}

/// Container information returned from listing.
#[derive(Debug, Clone)]
pub struct ContainerInfo {
    pub id: String,
    pub name: Option<String>,
    pub image: Option<String>,
    pub state: Option<String>,
    pub status: Option<String>,
}

/// List running containers via bollard.
pub async fn list_running_containers() -> Vec<ContainerInfo> {
    let docker = match Docker::connect_with_socket_defaults() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let options = ListContainersOptionsBuilder::new()
        .all(false)
        .build();
    match docker.list_containers(Some(options)).await {
        Ok(containers) => containers
            .into_iter()
            .filter_map(|c| {
                let names = c.names.unwrap_or_default();
                let name = names.first().map(|n| n.trim_start_matches('/').to_string());
                Some(ContainerInfo {
                    id: c.id.unwrap_or_default(),
                    name,
                    image: c.image,
                    state: c.state.as_ref().map(|s| format!("{:?}", s)),
                    status: c.status,
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Inspect a container via bollard and return its health status.
pub async fn container_health_status(container_id: &str) -> Option<String> {
    let docker = Docker::connect_with_socket_defaults().ok()?;
    let options = InspectContainerOptionsBuilder::new()
        .build();
    let inspection = docker.inspect_container(container_id, Some(options)).await.ok()?;
    inspection
        .state
        .and_then(|s| s.health)
        .and_then(|h| h.status)
        .map(|s| format!("{:?}", s))
}

/// Start a container via dockworker as a fallback API.
pub async fn start_container_dockworker(container_id: &str) -> bool {
    let api = match DockworkerDocker::connect_with_defaults() {
        Ok(a) => a,
        Err(_) => return false,
    };
    api.start_container(container_id).await.is_ok()
}

/// Create a container via dockworker.
pub async fn create_container_dockworker(name: &str, image: &str) -> Option<String> {
    let api = DockworkerDocker::connect_with_defaults().ok()?;
    let options = ContainerCreateOptions::new(image);
    match api.create_container(Some(name), &options).await {
        Ok(resp) => Some(resp.id),
        Err(_) => None,
    }
}

/// Stop a container via bollard.
pub async fn stop_container(container_id: &str) -> bool {
    let docker = match Docker::connect_with_socket_defaults() {
        Ok(d) => d,
        Err(_) => return false,
    };
    docker.stop_container(container_id, None).await.is_ok()
}
