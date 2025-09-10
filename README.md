# Qebbeq - Container Lifecycle Manager
Qebbeq addresses several challenges commonly encountered when building CI/CD pipelines for GitOps-managed infrastructure:

- **Efficient Image Cleanup:** Container images are generated frequently, but storage resources are finite. Qebbeq automatically cleans up the registry by removing images that are no longer referenced in the local cluster, helping to conserve space.
- **Image Promotion Across Stages:** Images are built and promoted through various environments (e.g., development, testing, production). Qebbeq facilitates the transfer of images between local registries, streamlining the promotion process across stages.
- **On-Demand Image Building:** GitOps defines the desired state of infrastructure, which can conflict with traditional pipeline-driven workflows. Qebbeq enables on-demand image building by allowing references to version tags or Git SHAs, ensuring that the correct images are available when needed.
- **Mirroring and Reliability:** Many images originate from public registries such as Docker Hub, which are subject to rate limits and potential removal of images. Qebbeq acts as a local mirror for these images, applying the same cleanup mechanisms to ensure reliability and availability.

By automating these processes, Qebbeq helps maintain a clean, reliable, and efficient container image lifecycle within GitOps workflows.

## Name

**Qebbeq** is derived from Klingon (*qeb* = fate, *beq* = crew).  
It can be understood as *“the crew of fate”* – those who operate under destiny’s command.  
In this project, it reflects the role of deciding which container images live on and which are removed.  

(And yes, this is just to solve one of the three hard problems in computer science: naming things.)

## License

MIT License. See `LICENSE` for details.

## Custom Resources

### Image (qebbeq.tuco86.dev/v1alpha1)
Tracks a concrete image reference observed in workload specs.
Fields:
* spec.image
* status.references (set of referencing objects)
* status.mirrored (bool, placeholder until real mirroring implemented)
* status.last_mirror_time
* status.last_unreferenced
* status.conditions (Ready)

### ImageMirror (qebbeq.tuco86.dev/v1alpha1)
Configures upstream repository access & optional polling.
Fields:
* spec.upstreamRegistry
* spec.repository
* spec.policy: IfNotPresent | Poll(intervalSeconds)
* status.lastSyncTime
* status.conditions (Ready, etc.)

### Generate CRDs
```
qebbeq --print-crd
```
Prints multi-document YAML for Image and ImageMirror.

## Roadmap
* Integrate real mirroring (crane) instead of placeholder mirrored=true.
* Error/backoff & detailed conditions for failures.
* Tag discovery and on-demand copy when referenced.
* Metrics & observability for mirror/pull success rates.
