# Assigning Pods to Nodes

You can constrain a Pod so that it is restricted to run on particular node(s), or to prefer to run on particular nodes. There are several ways to do this and the recommended approaches all use label selectors to facilitate the selection.

## Methods for Assigning Pods to Nodes

You can use any of the following methods to choose where Kubernetes schedules specific Pods:

- `nodeSelector` field matching against node labels
- Affinity and anti-affinity
- `nodeName` field
- Pod topology spread constraints

## Node Labels

Like many other Kubernetes objects, nodes have labels. Kubernetes populates a standard set of labels on all nodes in a cluster. You can also attach labels manually.

### Node Isolation/Restriction

Adding labels to nodes allows you to target Pods for scheduling on specific nodes or groups of nodes. If you use labels for node isolation, choose label keys that the kubelet cannot modify.

The `NodeRestriction` admission plugin prevents the kubelet from setting or modifying labels with a `node-restriction.kubernetes.io/` prefix.

To use this label prefix for node isolation:

1. Ensure you are using the Node authorizer and have enabled the `NodeRestriction` admission plugin.
2. Add labels with the `node-restriction.kubernetes.io/` prefix to your nodes and use those labels in your node selectors (e.g., `example.com.node-restriction.kubernetes.io/fips=true`).

## nodeSelector

`nodeSelector` is the simplest recommended form of node selection constraint. You can add the `nodeSelector` field to your Pod specification and specify the node labels you want the target node to have. Kubernetes only schedules the Pod onto nodes that have each of the labels you specify.

## Affinity and Anti-affinity

`nodeSelector` is the simplest way to constrain Pods to nodes with specific labels. Affinity and anti-affinity expand the types of constraints you can define.

### Benefits of Affinity and Anti-affinity:

- The affinity/anti-affinity language is more expressive than `nodeSelector`
- You can indicate that a rule is soft or preferred
- You can constrain a Pod using labels on other Pods running on the node (inter-pod affinity/anti-affinity)

### Node Affinity

Node affinity is conceptually similar to `nodeSelector`, but more expressive and allows you to specify soft rules.

**Two types of node affinity:**

- `requiredDuringSchedulingIgnoredDuringExecution`: The scheduler can't schedule the Pod unless the rule is met (like `nodeSelector`, but more expressive)
- `preferredDuringSchedulingIgnoredDuringExecution`: The scheduler tries to find a node that meets the rule; if unavailable, the scheduler still schedules the Pod

> **Note:** `IgnoredDuringExecution` means that if node labels change after Kubernetes schedules the Pod, the Pod continues to run.

### Node Affinity Example

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: with-node-affinity
spec:
  affinity:
    nodeAffinity:
      requiredDuringSchedulingIgnoredDuringExecution:
        nodeSelectorTerms:
        - matchExpressions:
          - key: topology.kubernetes.io/zone
            operator: In
            values:
            - antarctica-east1
            - antarctica-west1
      preferredDuringSchedulingIgnoredDuringExecution:
      - weight: 1
        preference:
          matchExpressions:
          - key: another-node-label-key
            operator: In
            values:
            - another-node-label-value
  containers:
  - name: with-node-affinity
    image: registry.k8s.io/pause:3.8
```

In this example:
- The node **must** have a label with key `topology.kubernetes.io/zone` with value `antarctica-east1` or `antarctica-west1`
- The node **preferably** has a label with key `another-node-label-key` and value `another-node-label-value`

### Affinity Operators

You can use the `operator` field to specify a logical operator: `In`, `NotIn`, `Exists`, `DoesNotExist`, `Gt`, and `Lt`.

**Notes:**
- If you specify both `nodeSelector` and `nodeAffinity`, both must be satisfied for the Pod to be scheduled
- Multiple terms in `nodeSelectorTerms` are ORed
- Multiple expressions in a single `matchExpressions` field are ANDed
- `NotIn` and `DoesNotExist` allow you to define node anti-affinity behavior

### Node Affinity Weight

You can specify a `weight` between 1 and 100 for each instance of `preferredDuringSchedulingIgnoredDuringExecution`. The scheduler sums the weights of all preferred rules that the node satisfies and adds this to the overall scheduling score.

## Inter-pod Affinity and Anti-affinity

Inter-pod affinity and anti-affinity allow you to constrain Pods based on labels of other Pods running on nodes, rather than just node labels.

## nodeName

`nodeName` is a simple field that allows you to specify the exact node on which the Pod should run. If the specified node doesn't exist or doesn't have capacity, the Pod fails.

## nominatedNodeName

This field indicates the node to which the scheduler has offered binding of the Pod. This is set during the preemption process.

## Pod Topology Spread Constraints

Pod topology spread constraints allow you to spread Pods across a cluster according to defined topology domains (e.g., zones, nodes, regions).

## Pod Topology Labels

Kubernetes provides standard labels for pod topology that can be used with topology spread constraints and affinity rules.
