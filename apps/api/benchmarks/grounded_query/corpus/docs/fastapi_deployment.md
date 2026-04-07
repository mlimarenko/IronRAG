# Deployments Concepts - FastAPI Documentation

## Overview

When deploying a FastAPI application, several key concepts affect how you architect your deployment:

- **Security - HTTPS**
- **Running on startup**
- **Restarts**
- **Replication** (number of processes running)
- **Memory**
- **Previous steps before starting**

The ultimate objective is to serve your API clients securely, avoid disruptions, and use compute resources efficiently.

---

## Security - HTTPS

HTTPS is provided by a component **external** to your application server: a **TLS Termination Proxy**. This component is also responsible for renewing HTTPS certificates.

### Example Tools for HTTPS

- **Traefik** - Automatically handles certificate renewals
- **Caddy** - Automatically handles certificate renewals
- **Nginx** - Requires external component like Certbot for renewals
- **HAProxy** - Requires external component like Certbot for renewals
- **Kubernetes with Nginx Ingress** - Uses cert-manager for renewals
- **Cloud provider services** - Handle HTTPS internally

---

## Program and Process

### What is a Program

A **program** can refer to:
- The Python code you write
- The executable file (e.g., `python`, `uvicorn`)
- A program running on the OS using CPU and memory (also called a **process**)

### What is a Process

A **process** is specifically:
- A program running on the operating system
- Managed by the OS, using CPU and RAM
- Can be terminated or killed, stopping execution
- Multiple processes of the same program can run simultaneously

---

## Running on Startup

### In a Remote Server

Running `fastapi run` manually works during development, but:
- If your connection is lost, the process dies
- Server restarts go unnoticed, and your API stays dead

### Run Automatically on Startup

Your server program should start automatically on server startup without human intervention.

### Example Tools to Run at Startup

- Docker
- Kubernetes
- Docker Compose
- Docker in Swarm Mode
- Systemd
- Supervisor
- Cloud provider services

---

## Restarts

### We Make Mistakes

Software always has bugs. As developers, we continuously improve code and fix issues.

### Small Errors Automatically Handled

FastAPI contains errors to individual requests. A client gets a **500 Internal Server Error** for that request, but the application continues serving other requests.

### Bigger Errors - Crashes

Some code can crash the entire application, making Uvicorn and Python crash entirely.

### Restart After Crash

An external component should handle automatic restarts. Since the process crashed, nothing in the application code can fix it.

### Example Tools to Restart Automatically

The same tools that run programs on startup typically handle restarts:
- Docker
- Kubernetes
- Docker Compose
- Docker in Swarm Mode
- Systemd
- Supervisor
- Cloud provider services

---

## Replication - Processes and Memory

### Multiple Processes - Workers

With multiple CPU cores and many clients, run **multiple worker processes** of the same application to distribute requests among them.

### Worker Processes and Ports

Only one process can listen on one combination of port and IP address. With multiple processes, a **single process listens on the port** and transmits communication to each worker process.

### Memory per Process

Each process has its own memory space. Multiple processes **don't share memory**. If your code loads a 1 GB machine learning model:
- **1 worker**: 1 GB RAM
- **4 workers**: 4 GB RAM total

### Server Memory

Plan accordingly. For example, if your server has 3 GB RAM and each worker needs 1 GB, you can only run 3 workers safely.

### Multiple Processes - An Example

```
Manager Process (listens on port) -> distributes -> Worker Process 1
                                  -> distributes -> Worker Process 2
```

Each worker process:
- Runs your application
- Performs computations
- Loads variables in RAM

### Examples of Replication Tools and Strategies

- **Uvicorn with `--workers`** - One Uvicorn process manager listens on IP:port and starts multiple worker processes
- **Kubernetes** - Multiple containers, each with one Uvicorn process
- **Cloud services** - Handle replication automatically; you provide a single process or container image

---

## Previous Steps Before Starting

### Overview

You may need to perform steps **before starting** your application, such as database migrations.

**Important**: These steps should run only **once**, in a **single process** to avoid:
- Duplicated work
- Conflicts (especially with delicate operations like migrations)

### Examples of Previous Steps Strategies

- **Kubernetes Init Containers** - Run before your app container
- **Bash script** - Runs previous steps, then starts your application
- **Custom startup logic** - Depends on your deployment strategy

---

## Resource Utilization

Your server resources include:
- **CPU computation time**
- **RAM memory**
- Disk space
- Network bandwidth

### Balance

- **Too low utilization**: Wasting money and power
- **100% utilization**: Risk of crashes or slowdowns
- **Optimal target**: 50-90% resource utilization

### Monitoring

Use tools like `htop` to monitor CPU and RAM, or use more complex distributed monitoring tools.

---

## Recap

Key concepts for deployment decisions:

1. **Security - HTTPS** - Use a TLS Termination Proxy
2. **Running on startup** - Automate program startup
3. **Restarts** - Handle automatic restarts after crashes
4. **Replication** - Run multiple worker processes
5. **Memory** - Plan RAM per process
6. **Previous steps** - Run one-time setup in single process

Understanding these concepts provides the intuition needed to configure and tweak deployments effectively.
