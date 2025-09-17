# Copilot Instructions for Qebbeq

## Project Overview
Qebbeq is a Rust-based Kubernetes controller for managing container image lifecycles in GitOps workflows. It automates image cleanup, mirroring, and promotion between environments, focusing on reliability and efficient storage use.

## Architecture & Key Components
- **src/main.rs**: Entry point; sets up the controller runtime and resource watchers.
- **src/resources/**: Contains custom resource definitions and logic:
  - `image.rs`: Defines the `Image` CRD and its status fields.
  - `imagemirror.rs`: Defines the `ImageMirror` CRD and its status fields.
  - `mod.rs`: Module organization for resources.
- **helm/**: Helm chart for deploying Qebbeq to Kubernetes. Key templates:
  - `controller-deployment.yaml`: Controller deployment spec.
  - `registry-deployment.yaml`: Registry deployment spec.
- **image-mirror-example.yaml**: Example manifest for `ImageMirror` CRD.

## Developer Workflows
- **Build**: Use `cargo build` to compile the project.
- **Run**: Use `cargo run` for local execution. For CRD YAML generation, run:
  ```pwsh
  cargo run -- --print-crd
  ```
- **Test**: Use `cargo test` for unit/integration tests.
- **Deploy**: Use Helm chart in `helm/` for Kubernetes deployment. Customize values in `values.yaml`.

## Patterns & Conventions
- **CRD Definitions**: All custom resources are defined in `src/resources/`. Status fields track references, mirroring, and sync times.
- **Image Mirroring**: Mirroring logic is currently a placeholder; see roadmap for future integration with real tools (e.g., `crane`).
- **Error Handling**: Status conditions (e.g., `Ready`) are used for reporting resource state and errors.
- **Promotion & Cleanup**: Images are promoted and cleaned up based on cluster references and policies defined in CRDs.

## Integration Points
- **Kubernetes API**: Interacts with cluster resources via CRDs.
- **Container Registries**: Supports mirroring and cleanup for local and upstream registries.
- **Helm**: Used for deployment and configuration.

## Examples
- To generate CRD YAML:
  ```pwsh
  cargo run -- --print-crd
  ```
- To deploy with Helm:
  ```pwsh
  helm install qebbeq ./helm -f ./helm/values.yaml
  ```

## Roadmap Highlights
- Replace placeholder mirroring with real registry sync (e.g., `crane`).
- Add metrics and observability for image operations.
- Improve error handling and backoff strategies.

---
**Reference files:**
- `src/main.rs`, `src/resources/image.rs`, `src/resources/imagemirror.rs`, `helm/`, `README.md`

For questions or unclear conventions, review the README or ask for clarification.
