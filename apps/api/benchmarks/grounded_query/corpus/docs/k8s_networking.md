# Cluster Networking

Networking is a central part of Kubernetes, but it can be challenging to understand exactly how it is expected to work. There are 4 distinct networking problems to address:

1. **Highly-coupled container-to-container communications**: solved by Pods and `localhost` communications.
2. **Pod-to-Pod communications**: the primary focus of this document.
3. **Pod-to-Service communications**: covered by Services.
4. **External-to-Service communications**: also covered by Services.

Kubernetes is all about sharing machines among applications. Typically, sharing machines requires ensuring that two applications do not try to use the same ports. Coordinating ports across multiple developers is very difficult to do at scale and exposes users to cluster-level issues outside of their control.

Dynamic port allocation brings a lot of complications to the system - every application has to take ports as flags, the API servers have to know how to insert dynamic port numbers into configuration blocks, services have to know how to find each other, etc. Rather than deal with this, Kubernetes takes a different approach.

## The Kubernetes Networking Model

Kubernetes imposes the following fundamental requirements on any networking implementation (barring any intentional network segmentation policies):

- Every Pod in a cluster gets its own unique cluster-wide IP address. This means you do not need to explicitly create links between Pods and you almost never need to deal with mapping container ports to host ports.
- Pods can communicate with all other Pods on any other node without NAT.
- Agents on a node (e.g. system daemons, kubelet) can communicate with all Pods on that node.
- Pods in the host network of a node can communicate with all Pods on all nodes without NAT.

This model is not only less complex overall, but it is principally compatible with the desire for Kubernetes to enable low-friction porting of apps from VMs to containers. If your job previously ran in a VM, your VM had an IP and could talk to other VMs in your project. This is the same basic model.

## Kubernetes IP Address Ranges

Kubernetes clusters require to allocate non-overlapping IP addresses for Pods, Services and Nodes, from a range of available addresses configured in the following components:

- The network plugin is configured to assign IP addresses to Pods.
- The kube-apiserver is configured to assign IP addresses to Services.
- The kubelet or the cloud-controller-manager is configured to assign IP addresses to Nodes.

## Cluster Networking Types

Kubernetes clusters, attending to the IP families configured, can be categorized into:

- **IPv4 only**: The network plugin, kube-apiserver and kubelet/cloud-controller-manager are configured to assign only IPv4 addresses.
- **IPv6 only**: The network plugin, kube-apiserver and kubelet/cloud-controller-manager are configured to assign only IPv6 addresses.
- **IPv4/IPv6 or IPv6/IPv4 dual-stack**:
  - The network plugin is configured to assign IPv4 and IPv6 addresses.
  - The kube-apiserver is configured to assign IPv4 and IPv6 addresses.
  - The kubelet or cloud-controller-manager is configured to assign IPv4 and IPv6 addresses.
  - All components must agree on the configured primary IP family.

Kubernetes clusters only consider the IP families present on the Pods, Services and Nodes objects, independently of the existing IPs of the represented objects. For example, a server or a pod can have multiple IP addresses assigned to its interfaces, but only the IP addresses in `node.status.addresses` or `pod.status.ips` are considered when implementing the Kubernetes network model and defining the cluster type.

## How to Implement the Kubernetes Network Model

The network model is implemented by the container runtime on each node. The most common container runtimes use Container Network Interface (CNI) plugins to manage their network and security capabilities. Many different CNI plugins exist from many different vendors. Some of these provide only basic features of adding and removing network interfaces, while others provide more sophisticated solutions, such as integration with other container orchestration systems, running multiple CNI plugins, advanced IPAM features etc.

### Common CNI Plugins

The Kubernetes ecosystem offers a variety of CNI plugins for implementing cluster networking:

- **Calico**: Provides networking and network policy enforcement. Supports both overlay and non-overlay networking modes.
- **Cilium**: Uses eBPF for high-performance networking and security. Provides advanced network policies and observability.
- **Flannel**: A simple overlay network that satisfies the Kubernetes networking requirements. Good for getting started.
- **Weave Net**: Creates a virtual network that connects Docker containers across multiple hosts and enables automatic discovery.
- **Canal**: Combines Flannel for networking with Calico for network policy.
- **Antrea**: Built on Open vSwitch, provides networking and security features for Kubernetes clusters.

### Network Policies

Kubernetes NetworkPolicy resources allow you to control traffic flow between Pods and between Pods and external endpoints. Network policies are implemented by the CNI plugin, not all plugins support them.

A basic NetworkPolicy example:

```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: test-network-policy
  namespace: default
spec:
  podSelector:
    matchLabels:
      role: db
  policyTypes:
  - Ingress
  - Egress
  ingress:
  - from:
    - ipBlock:
        cidr: 172.17.0.0/16
        except:
        - 172.17.1.0/24
    - namespaceSelector:
        matchLabels:
          project: myproject
    - podSelector:
        matchLabels:
          role: frontend
    ports:
    - protocol: TCP
      port: 6379
  egress:
  - to:
    - ipBlock:
        cidr: 10.0.0.0/24
    ports:
    - protocol: TCP
      port: 5978
```

This NetworkPolicy:
- Applies to Pods with label `role: db` in the `default` namespace
- Allows ingress traffic on TCP port 6379 from:
  - IP addresses in the `172.17.0.0/16` range (except `172.17.1.0/24`)
  - Pods in namespaces with the label `project: myproject`
  - Pods with the label `role: frontend`
- Allows egress traffic to the `10.0.0.0/24` CIDR on TCP port 5978

### DNS in Kubernetes

Kubernetes creates DNS records for Services and Pods. You can contact Services with consistent DNS names instead of IP addresses.

**Service DNS records:**
- A/AAAA records: `<service-name>.<namespace>.svc.cluster.local`
- SRV records: `_<port-name>._<protocol>.<service-name>.<namespace>.svc.cluster.local`

**Pod DNS records:**
- A/AAAA records: `<pod-ip-with-dashes>.<namespace>.pod.cluster.local`

Example: A Service named `my-svc` in namespace `my-ns` gets a DNS A record for `my-svc.my-ns.svc.cluster.local`.

### kube-proxy Modes

kube-proxy is responsible for implementing a form of virtual IP for Services. kube-proxy supports several proxy modes:

- **iptables mode** (default): Uses Linux iptables rules to handle traffic. Good for clusters with moderate numbers of Services.
- **IPVS mode**: Uses Linux IPVS (IP Virtual Server) for load balancing. Better performance for clusters with large numbers of Services.
- **nftables mode**: Uses Linux nftables as the backend. Modern alternative to iptables mode.

To check the current proxy mode:

```bash
kubectl get configmap kube-proxy -n kube-system -o yaml | grep mode
```

## Service Mesh

A service mesh provides additional networking capabilities beyond what the basic Kubernetes networking model offers:

- **Mutual TLS (mTLS)**: Encrypt all service-to-service communication
- **Traffic management**: Advanced routing, retries, circuit breaking
- **Observability**: Distributed tracing, metrics, logging
- **Access control**: Fine-grained authorization policies

Popular service mesh implementations include Istio, Linkerd, and Consul Connect.

## Troubleshooting Networking

Common kubectl commands for debugging networking issues:

```bash
# Check Pod IP addresses
kubectl get pods -o wide

# Check Service endpoints
kubectl get endpoints <service-name>

# Check EndpointSlices
kubectl get endpointslices

# Debug DNS resolution from within a Pod
kubectl exec -it <pod-name> -- nslookup <service-name>

# Check network policies
kubectl get networkpolicies

# Check kube-proxy logs
kubectl logs -n kube-system -l k8s-app=kube-proxy

# Test connectivity between Pods
kubectl exec -it <pod-name> -- curl <target-service>:<port>
```

## What's Next

The early design of the networking model and its rationale are described in more detail in the networking design document. For future plans and some on-going efforts that aim to improve Kubernetes networking, please refer to the SIG-Network KEPs.
