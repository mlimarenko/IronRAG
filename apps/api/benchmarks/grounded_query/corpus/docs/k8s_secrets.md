# Kubernetes Secrets

## Overview

A Secret is an object that contains a small amount of sensitive data such as a password, a token, or a key. Secrets allow you to store confidential information separately from Pod specifications and container images, reducing the risk of exposure during workflows.

**Key Points:**
- Secrets can be created independently of Pods that use them
- Similar to ConfigMaps but specifically intended for confidential data
- By default stored unencrypted in etcd

### Security Caution

Kubernetes Secrets are stored unencrypted by default. To safely use Secrets:

1. Enable Encryption at Rest for Secrets
2. Enable or configure RBAC rules with least-privilege access
3. Restrict Secret access to specific containers
4. Consider using external Secret store providers

## Uses for Secrets

Common purposes for Secrets:

- Set environment variables for containers
- Provide credentials such as SSH keys or passwords to Pods
- Allow the kubelet to pull container images from private registries
- Bootstrap token Secrets for node registration automation

### Use Case: Dotfiles in a Secret Volume

Make data "hidden" by defining a key that begins with a dot. This represents a dotfile or "hidden" file.

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: dotfile-secret
data:
  .secret-file: dmFsdWUtMg0KDQo=
---
apiVersion: v1
kind: Pod
metadata:
  name: secret-dotfiles-pod
spec:
  volumes:
    - name: secret-volume
      secret:
        secretName: dotfile-secret
  containers:
    - name: dotfile-test-container
      image: registry.k8s.io/busybox
      command:
        - ls
        - "-l"
        - "/etc/secret-volume"
      volumeMounts:
        - name: secret-volume
          readOnly: true
          mountPath: "/etc/secret-volume"
```

> **Note:** Files beginning with dot characters are hidden from `ls -l` output; use `ls -la` to see them.

### Use Case: Secret Visible to One Container in a Pod

Divide complex applications into multiple containers with different privilege levels:
- **Frontend container**: Handles user interaction and business logic (no access to private key)
- **Signer container**: Can see the private key and responds to signing requests

This partitioned approach limits the exposure if one container is compromised.

### Alternatives to Secrets

Consider these alternatives depending on your use case:

- **ServiceAccount tokens**: For authentication between applications within the same cluster
- **Third-party tools**: External services that manage sensitive data and reveal Secrets upon proper authentication
- **CertificateSigningRequests**: For X.509 certificate authentication
- **Device plugins**: Expose node-local encryption hardware (e.g., Trusted Platform Module) to specific Pods
- **Operators with external services**: Fetch short-lived session tokens and manage their lifecycle

You can combine multiple approaches, including Secret objects themselves.

## Types of Secret

When creating a Secret, specify its type using the `type` field. Kubernetes provides several built-in types:

| Built-in Type | Usage |
|---|---|
| `Opaque` | arbitrary user-defined data |
| `kubernetes.io/service-account-token` | ServiceAccount token |
| `kubernetes.io/dockercfg` | serialized `~/.dockercfg` file |
| `kubernetes.io/dockerconfigjson` | serialized `~/.docker/config.json` file |
| `kubernetes.io/basic-auth` | credentials for basic authentication |
| `kubernetes.io/ssh-auth` | credentials for SSH authentication |
| `kubernetes.io/tls` | data for a TLS certificate and key |
| `bootstrap.kubernetes.io/token` | bootstrap token data |

### Opaque Secrets

The default type. Used for arbitrary user-defined data, typically in base64 format.

### ServiceAccount Token Secrets

Contains a token that identifies a ServiceAccount. Automatically created when you create a ServiceAccount.

### Docker Config Secrets

Store serialized Docker configuration (`~/.dockercfg` or `~/.docker/config.json`) for authenticating with Docker registries.

**Types:**
- `kubernetes.io/dockercfg`: Uses `~/.dockercfg` format
- `kubernetes.io/dockerconfigjson`: Uses `~/.docker/config.json` format

### Basic Authentication Secret

Stores username and password for basic HTTP authentication.

**Required fields in `data`:**
- `username`: The user name for authentication
- `password`: The password or token for authentication

### SSH Authentication Secrets

Store data for SSH authentication.

**Required field in `data`:**
- `ssh-privatekey`: The private SSH key to use for authentication

### TLS Secrets

Store TLS certificate and associated key.

**Required fields in `data`:**
- `tls.crt`: The certificate or certificate chain (in PEM format)
- `tls.key`: The private key associated with the given certificate (in PEM format)

### Bootstrap Token Secrets

Automate node registration by storing bootstrap tokens.

## Working with Secrets

### Creating a Secret

**Using kubectl:**

```bash
kubectl create secret generic my-secret --from-literal=key1=value1 --from-literal=key2=value2
```

**From a file:**

```bash
kubectl create secret generic my-secret --from-file=./username.txt --from-file=./password.txt
```

**Using a YAML manifest:**

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: mysecret
type: Opaque
data:
  username: YWRtaW4=
  password: MWYyZDFlMmU2N2Rm
```

### Editing a Secret

```bash
kubectl edit secret mysecret
```

### Using a Secret

Secrets can be used in Pods in two main ways:

1. **As files** in a volume mounted in one or more of its containers
2. **As environment variables**

### Using Secrets as Files from a Pod

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: mypod
spec:
  containers:
  - name: mycontainer
    image: redis
    volumeMounts:
    - name: foo
      mountPath: "/etc/foo"
      readOnly: true
  volumes:
  - name: foo
    secret:
      secretName: mysecret
```

Each key in the `data` map of the Secret becomes a filename in the volume.

### Using Secrets as Environment Variables

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: secret-env-pod
spec:
  containers:
  - name: mycontainer
    image: redis
    env:
      - name: SECRET_USERNAME
        valueFrom:
          secretKeyRef:
            name: mysecret
            key: username
      - name: SECRET_PASSWORD
        valueFrom:
          secretKeyRef:
            name: mysecret
            key: password
```

### Container Image Pull Secrets

Use Secrets to provide credentials for pulling container images from private registries.

**Create a Secret for Docker authentication:**

```bash
kubectl create secret docker-registry myregistrykey \
  --docker-server=DOCKER_REGISTRY_SERVER \
  --docker-username=DOCKER_USER \
  --docker-password=DOCKER_PASSWORD \
  --docker-email=DOCKER_EMAIL
```

**Use in Pod spec:**

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: foo
spec:
  containers:
    - name: foo
      image: janedoe/awesomeapp:v1
  imagePullSecrets:
    - name: myregistrykey
```

### Using Secrets with Static Pods

Secrets cannot be used with static Pods created by the kubelet.

## Immutable Secrets

Create immutable Secrets to prevent accidental (or malicious) updates that could cause application outages.

### Marking a Secret as Immutable

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: my-immutable-secret
data:
  key1: value1
immutable: true
```

Once a Secret is marked as immutable, the field cannot be disabled, and the contents cannot be modified.

**Benefits:**
- Prevents accidental updates that could affect cluster stability
- Improves cluster performance by reducing load on etcd
- Protects against unwanted changes by privileged users

## Information Security for Secrets

### Configure Least-Privilege Access to Secrets

Use RBAC to restrict who can access Secrets:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: secret-reader
rules:
- apiGroups: [""]
  resources: ["secrets"]
  verbs: ["get", "list"]
  resourceNames: ["mysecret"]  # Limit to specific Secrets
```

**Best practices:**
1. Grant the minimum permissions necessary
2. Use resource names to limit access to specific Secrets
3. Regularly audit Secret access
4. Encrypt Secrets at rest
5. Restrict which containers can access which Secrets
6. Consider external secret management solutions
