# Kubernetes Service

## Services in Kubernetes

A **Service** in Kubernetes is an abstraction that exposes a network application running as one or more Pods in your cluster. It provides a single outward-facing endpoint even when the workload is split across multiple backends.

### Key Purpose

Services solve the problem of Pod ephemeracy. Since Pods are created and destroyed dynamically, their IP addresses change constantly. Services provide a stable network endpoint that remains consistent regardless of which Pods are running behind it.

### Cloud-native Service Discovery

If your application can use Kubernetes APIs, you can query the API server for matching EndpointSlices. Kubernetes automatically updates EndpointSlices whenever the set of Pods in a Service changes.

## Defining a Service

A Service is a Kubernetes object defined in YAML. Here's a basic example:

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  selector:
    app.kubernetes.io/name: MyApp
  ports:
    - protocol: TCP
      port: 80
      targetPort: 9376
```

This manifest creates a Service named "my-service" that:
- Targets Pods with label `app.kubernetes.io/name: MyApp`
- Listens on TCP port 80
- Forwards traffic to port 9376 on the target Pods
- Is assigned a cluster IP automatically

### Port Definitions with Named Ports

You can reference named ports in Pods:

```yaml
apiVersion: v1
kind: Service
metadata:
  name: nginx-service
spec:
  selector:
    app.kubernetes.io/name: proxy
  ports:
  - name: name-of-service-port
    protocol: TCP
    port: 80
    targetPort: http-web-svc

---
apiVersion: v1
kind: Pod
metadata:
  name: nginx
  labels:
    app.kubernetes.io/name: proxy
spec:
  containers:
  - name: nginx
    image: nginx:stable
    ports:
      - containerPort: 80
        name: http-web-svc
```

### Services Without Selectors

Services can be defined without selectors, useful for:
- Routing to external databases
- Pointing to services in other namespaces
- Migrating applications to Kubernetes

For these cases, you manually manage EndpointSlices or create Endpoints resources.

### EndpointSlices

EndpointSlices are the current mechanism for tracking Service endpoints. Each EndpointSlice contains references to a set of Pod endpoints and is automatically updated by the Service controller.

### Multi-port Services

Services can expose multiple ports:

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  selector:
    app.kubernetes.io/name: MyApp
  ports:
  - name: http
    protocol: TCP
    port: 80
    targetPort: 9376
  - name: https
    protocol: TCP
    port: 443
    targetPort: 9377
```

## Service Types

### ClusterIP (Default)

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  type: ClusterIP
  selector:
    app.kubernetes.io/name: MyApp
  ports:
    - protocol: TCP
      port: 80
      targetPort: 9376
```

Exposes the Service on a cluster-internal IP. Only accessible from within the cluster.

### NodePort

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  type: NodePort
  selector:
    app.kubernetes.io/name: MyApp
  ports:
    - protocol: TCP
      port: 80
      targetPort: 9376
      nodePort: 30007
```

Exposes the Service on each Node's IP at a static port (the NodePort). Accessible externally via `<NodeIP>:<NodePort>`.

### LoadBalancer

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  type: LoadBalancer
  selector:
    app.kubernetes.io/name: MyApp
  ports:
    - protocol: TCP
      port: 80
      targetPort: 9376
```

Exposes the Service externally using a cloud provider's load balancer. Requires cloud provider integration.

### ExternalName

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  type: ExternalName
  externalName: my.database.example.com
  ports:
    - protocol: TCP
      port: 5432
```

Maps the Service to the contents of the `externalName` field (e.g., an external database). Returns a CNAME record.

## Headless Services

Headless Services don't allocate a cluster IP, used for:
- StatefulSets
- Service meshes
- Custom DNS behavior

### With Selectors

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  clusterIP: None
  selector:
    app.kubernetes.io/name: MyApp
  ports:
    - protocol: TCP
      port: 80
      targetPort: 9376
```

### Without Selectors

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  clusterIP: None
---
apiVersion: v1
kind: Endpoints
metadata:
  name: my-service
subsets:
  - addresses:
      - ip: 192.0.2.42
    ports:
      - port: 9376
```

## Discovering Services

### Environment Variables

When a Pod starts, Kubernetes injects environment variables for each Service:

```
MYAPP_SERVICE_HOST=10.0.0.3
MYAPP_SERVICE_PORT=9376
```

### DNS

Services are assigned DNS names: `<service-name>.<namespace>.svc.cluster.local`

Example: A Service named `my-service` in namespace `default` is accessible at:
```
my-service.default.svc.cluster.local
```

## Virtual IP Addressing Mechanism

Services use virtual IPs managed by kube-proxy, which:
- Intercepts traffic to the Service cluster IP
- Distributes traffic to backend Pods

### Traffic Policies

**Local**: Traffic only goes to Pods on the same node (preserves client IP)

```yaml
spec:
  trafficPolicy: Local
```

**Cluster** (default): Traffic can go to Pods on any node

### Session Stickiness

For stateful applications, enable client IP-based session affinity:

```yaml
spec:
  sessionAffinity: ClientIP
  sessionAffinityConfig:
    clientIP:
      timeoutSeconds: 10800
```

## External IPs

Expose a Service on specific external IPs:

```yaml
apiVersion: v1
kind: Service
metadata:
  name: my-service
spec:
  selector:
    app.kubernetes.io/name: MyApp
  externalIPs:
    - 80.11.12.10
  ports:
    - protocol: TCP
      port: 80
      targetPort: 9376
```

Traffic sent to `80.11.12.10:80` is routed to the Service.

## Key Naming Requirements

- Service names must be valid RFC 1035 label names (lowercase alphanumeric and hyphens)
- With the `RelaxedServiceNameValidation` feature gate enabled, names can start with digits and must be RFC 1123 compliant
