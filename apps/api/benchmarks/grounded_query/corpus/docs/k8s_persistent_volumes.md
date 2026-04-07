# Persistent Volumes

## Introduction

Managing storage is a distinct problem from managing compute instances. The PersistentVolume subsystem provides an API for users and administrators that abstracts details of how storage is provided from how it is consumed.

**Key Concepts:**

- **PersistentVolume (PV)**: A piece of storage in the cluster provisioned by an administrator or dynamically provisioned using Storage Classes. PVs have a lifecycle independent of any individual Pod that uses them.

- **PersistentVolumeClaim (PVC)**: A request for storage by a user, similar to how Pods request compute resources. PVCs consume PV resources and can request specific size and access modes.

- **StorageClass**: Enables cluster administrators to offer PersistentVolumes with varying properties (performance, capabilities) without exposing implementation details to users.

## Lifecycle of a Volume and Claim

### Provisioning

**Static Provisioning**: A cluster administrator creates PersistentVolumes in advance with details of the real storage.

**Dynamic Provisioning**: When no static PV matches a PVC, the cluster automatically provisions a volume based on StorageClasses. This requires:
- The PVC must request a storage class
- The administrator must have created and configured that class
- The `DefaultStorageClass` admission controller must be enabled on the API server

### Binding

A control loop watches for new PVCs and binds them to matching PVs. Key points:
- PVC to PV bindings are exclusive, one-to-one mappings
- Claims remain unbound indefinitely if no matching volume exists
- Once bound, the PV belongs to the user for as long as they need it

### Using

Pods use claims as volumes by including a `persistentVolumeClaim` section in their volume block:

```yaml
volumes:
  - name: storage
    persistentVolumeClaim:
      claimName: myclaim
```

### Storage Object in Use Protection

Ensures that PVCs in active use by a Pod and PVs bound to PVCs are not removed from the system, preventing data loss.

When a PVC is protected, its status shows `Terminating` with `kubernetes.io/pvc-protection` in the Finalizers list:

```bash
kubectl describe pvc hostpath
Name:          hostpath
Namespace:     default
StorageClass:  example-hostpath
Status:        Terminating
Finalizers:    [kubernetes.io/pvc-protection]
```

### Reclaiming

When a user is done with their volume, they can delete the PVC object from the API, which allows reclamation of the resource. The reclaim policy tells the cluster what to do with the volume after it has been released of its claim.

### PersistentVolume Deletion Protection Finalizer

A finalizer `kubernetes.io/pv-protection` prevents accidental deletion of PersistentVolumes that are in use.

### Reserving a PersistentVolume

Control loops bind PVCs to PVs. However, if a PVC is created for a specific PV using the `volumeName` field, it reserves that PV even before binding occurs.

### Expanding Persistent Volumes Claims

You can increase the size of a PVC by editing the `spec.resources.requests.storage` field. However:
- You cannot shrink a claim
- The underlying StorageClass must support expansion (check `allowVolumeExpansion: true`)
- The volume must support expansion

## Types of Persistent Volumes

Kubernetes supports various PersistentVolume types:
- awsElasticBlockStore
- cephfs
- cinder
- fc
- hostPath
- iscsi
- local
- nfs
- photonPersistentDisk
- quobyte
- rbd
- scaleIO
- storageos
- vsphereVolume
- And others...

## Persistent Volumes

### Capacity

PVs are requested with specific amounts of storage:

```yaml
spec:
  capacity:
    storage: 5Gi
```

### Volume Mode

Specifies how the volume will be consumed - `Filesystem` (default) or `Block`:

```yaml
spec:
  volumeMode: Filesystem  # or Block
```

### Access Modes

Defines how the volume can be mounted:
- **ReadWriteOnce (RWO)**: Mounted read-write by a single node
- **ReadOnlyMany (ROX)**: Mounted read-only by many nodes
- **ReadWriteMany (RWX)**: Mounted read-write by many nodes
- **ReadWriteOncePod (RWOP)**: Mounted read-write by a single Pod

```yaml
spec:
  accessModes:
    - ReadWriteOnce
```

### Class

Specifies a StorageClass name:

```yaml
spec:
  storageClassName: slow
```

### Reclaim Policy

Specifies what happens to the volume after being released:
- **Retain**: Administrator must manually reclaim
- **Delete**: Associated storage is deleted
- **Recycle**: Deprecated, runs `rm -rf` on the volume

```yaml
spec:
  persistentVolumeReclaimPolicy: Retain
```

### Mount Options

Additional mount options:

```yaml
spec:
  mountOptions:
    - hard
    - nfsvers=4.1
```

### Node Affinity

Restricts which nodes can access the volume:

```yaml
spec:
  nodeAffinity:
    required:
      nodeSelectorTerms:
      - matchExpressions:
        - key: kubernetes.io/hostname
          operator: In
          values:
          - specific-node
```

### Phase

PV phases indicate lifecycle stage:
- **Available**: Free resource
- **Bound**: Bound to a claim
- **Released**: Claim deleted but resource not reclaimed
- **Failed**: Volume failed automatic reclamation

## PersistentVolumeClaims

### Access Modes

Specify desired access mode:

```yaml
spec:
  accessModes:
    - ReadWriteOnce
```

### Volume Modes

Specify `Filesystem` or `Block`:

```yaml
spec:
  volumeMode: Filesystem
```

### Volume Name

Bind to a specific PV:

```yaml
spec:
  volumeName: pv-name
```

### Resources

Request storage capacity:

```yaml
spec:
  resources:
    requests:
      storage: 8Gi
```

### Selector

Use label selectors to filter PVs:

```yaml
spec:
  selector:
    matchLabels:
      release: stable
```

### Class

Reference a StorageClass:

```yaml
spec:
  storageClassName: fast
```

## Claims As Volumes

Pods reference PVCs in their volume specification:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: mypod
spec:
  containers:
    - name: myfrontend
      image: nginx
      volumeMounts:
        - mountPath: "/var/www/html"
          name: mypd
  volumes:
    - name: mypd
      persistentVolumeClaim:
        claimName: myclaim
```

### A Note on Namespaces

PVs are cluster-scoped resources, while PVCs are namespace-scoped. A Pod in one namespace cannot use a PVC from another namespace.

### PersistentVolumes typed `hostPath`

A `hostPath` PV uses a file or directory on the node. Use with caution as it creates scheduling dependencies:

```yaml
apiVersion: v1
kind: PersistentVolume
metadata:
  name: task-pv-volume
spec:
  storageClassName: manual
  capacity:
    storage: 10Gi
  accessModes:
    - ReadWriteOnce
  hostPath:
    path: "/mnt/data"
```

## Raw Block Volume Support

Some applications need raw block devices instead of filesystems.

### PersistentVolume using a Raw Block Volume

```yaml
apiVersion: v1
kind: PersistentVolume
metadata:
  name: block-pv
spec:
  capacity:
    storage: 10Gi
  volumeMode: Block
  accessModes:
    - ReadWriteOnce
  hostPath:
    path: /dev/sdb
```

### PersistentVolumeClaim requesting a Raw Block Volume

```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: raw-block-pvc
spec:
  accessModes:
    - ReadWriteOnce
  volumeMode: Block
  resources:
    requests:
      storage: 10Gi
```

### Pod specification adding Raw Block Device path in container

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: pod-with-block-volume
spec:
  containers:
    - name: fc-container
      image: fedora:latest
      deviceMounts:
      - devicePath: /dev/block
        name: data
  volumes:
    - name: data
      persistentVolumeClaim:
        claimName: raw-block-pvc
```

### Binding Block Volumes

If a user requests a block volume using the `volumeMode` field and specifies `Block`, the binding rules change slightly. Block volumes can only bind to block volumes, and filesystem volumes can only bind to filesystem volumes.

## Volume Snapshot and Restore Volume from Snapshot Support

### Create a PersistentVolumeClaim from a Volume Snapshot

```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: restored-pvc
spec:
  storageClassName: csi-hostpath-sc
  dataSource:
    name: snapshot-demo
    kind: VolumeSnapshot
    apiGroup: snapshot.storage.k8s.io
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: 1Gi
```

## Volume Cloning

### Create PersistentVolumeClaim from an existing PVC

```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: cloned-pvc
spec:
  storageClassName: csi-hostpath-sc
  dataSource:
    name: existing-src-pvc-name
    kind: PersistentVolumeClaim
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: 1Gi
```

## Volume Populators and Data Sources

The `dataSource` field enables populating a new volume with data from another source (volume snapshot, PVC, or other data sources registered with custom populators).

## Cross Namespace Data Sources

Data sources can reference volumes in other namespaces using:

```yaml
spec:
  dataSourceRef:
    apiGroup: snapshot.storage.k8s.io
    kind: VolumeSnapshot
    name: snapshot-demo
    namespace: other-namespace
```

## Writing Portable Configuration

For portable configuration across different clusters, avoid hardcoding specific PV implementations. Instead:
- Use StorageClass for dynamic provisioning
- Let the cluster administrator provide available storage options
- Use PVCs without specifying exact PV names

This allows your application to work across different cluster configurations without modification.
