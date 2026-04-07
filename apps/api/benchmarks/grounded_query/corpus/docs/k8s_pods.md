# Pods

## What is a Pod?

_Pods_ are the smallest deployable units of computing that you can create and manage in Kubernetes.

A _Pod_ (as in a pod of whales or pea pod) is a group of one or more containers, with shared storage and network resources, and a specification for how to run the containers. A Pod's contents are always co-located and co-scheduled, and run in a shared context. A Pod models an application-specific "logical host": it contains one or more application containers which are relatively tightly coupled.

As well as application containers, a Pod can contain init containers that run during Pod startup. You can also inject ephemeral containers for debugging a running Pod.

### Container Runtime Requirement

You need to install a container runtime into each node in the cluster so that Pods can run there.

The shared context of a Pod is a set of Linux namespaces, cgroups, and potentially other facets of isolation - the same things that isolate a container. Within a Pod's context, the individual applications may have further sub-isolations applied.

A Pod is similar to a set of containers with shared namespaces and shared filesystem volumes.

### Pod Usage Patterns

Pods in a Kubernetes cluster are used in two main ways:

1. **Pods that run a single container**: The "one-container-per-Pod" model is the most common Kubernetes use case; in this case, you can think of a Pod as a wrapper around a single container; Kubernetes manages Pods rather than managing the containers directly.

2. **Pods that run multiple containers that need to work together**: A Pod can encapsulate an application composed of multiple co-located containers that are tightly coupled and need to share resources. These co-located containers form a single cohesive unit.

Grouping multiple co-located and co-managed containers in a single Pod is a relatively advanced use case. You should use this pattern only in specific instances in which your containers are tightly coupled.

You don't need to run multiple containers to provide replication (for resilience or capacity); if you need multiple replicas, see Workload management.

## Using Pods

### Example Pod Configuration

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: nginx
spec:
  containers:
  - name: nginx
    image: nginx:1.14.2
    ports:
    - containerPort: 80
```

To create the Pod shown above, run the following command:

```bash
kubectl apply -f https://k8s.io/examples/pods/simple-pod.yaml
```

Pods are generally not created directly and are created using workload resources. See Working with Pods for more information on how Pods are used with workload resources.

### Workload Resources for Managing Pods

Usually you don't need to create Pods directly, even singleton Pods. Instead, create them using workload resources such as:

- **Deployment**: Manages a replicated application on your cluster
- **Job**: A finite or batch task that runs to completion
- **StatefulSet**: Manages deployment and scaling of a set of Pods, with durable storage and persistent identifiers for each Pod

Each Pod is meant to run a single instance of a given application. If you want to scale your application horizontally (to provide more overall resources by running more instances), you should use multiple Pods, one for each instance. In Kubernetes, this is typically referred to as _replication_. Replicated Pods are usually created and managed as a group by a workload resource and its controller.

Pods natively provide two kinds of shared resources for their constituent containers:
- **Networking**: Pod networking
- **Storage**: Pod storage

## Working with Pods

You'll rarely create individual Pods directly in Kubernetes -- even singleton Pods. This is because Pods are designed as relatively ephemeral, disposable entities. When a Pod gets created (directly by you, or indirectly by a controller), the new Pod is scheduled to run on a Node in your cluster. The Pod remains on that node until the Pod finishes execution, the Pod object is deleted, the Pod is _evicted_ for lack of resources, or the node fails.

### Important Note

Restarting a container in a Pod should not be confused with restarting a Pod. A Pod is not a process, but an environment for running container(s). A Pod persists until it is deleted.

### Pod Naming Requirements

The name of a Pod must be a valid DNS subdomain value, but this can produce unexpected results for the Pod hostname. For best compatibility, the name should follow the more restrictive rules for a DNS label.

## Pod OS

**FEATURE STATE:** `Kubernetes v1.25 [stable]`

You should set the `.spec.os.name` field to either `windows` or `linux` to indicate the OS on which you want the pod to run. These two are the only operating systems supported for now by Kubernetes. In the future, this list may be expanded.

In Kubernetes v1.35, the value of `.spec.os.name` does not affect how the kube-scheduler picks a node for the Pod to run on. In any cluster where there is more than one operating system for running nodes, you should:

1. Set the `kubernetes.io/os` label correctly on each node
2. Define pods with a `nodeSelector` based on the operating system label

The kube-scheduler assigns your pod to a node based on other criteria and may or may not succeed in picking a suitable node placement where the node OS is right for the containers in that Pod. The Pod security standards also use this field to avoid enforcing policies that aren't relevant to the operating system.

## Pods and Controllers

You can use workload resources to create and manage multiple Pods for you. A controller for the resource handles replication and rollout and automatic healing in case of Pod failure. For example, if a Node fails, a controller notices that Pods on that Node have stopped working and creates a replacement Pod. The scheduler places the replacement Pod onto a healthy Node.

Here are some examples of workload resources that manage one or more Pods:

- **Deployment**: Manages a replicated application on your cluster
- **StatefulSet**: Manages deployment and scaling of a set of Pods, with durable storage and persistent identifiers for each Pod
- **DaemonSet**: Ensures a copy of a Pod is running across a set of nodes in a cluster
