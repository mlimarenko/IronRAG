# Docker (Software)

**Docker** is a containerization platform that leverages operating system-level virtualization to package and deploy applications in lightweight containers, ensuring consistent execution across diverse computing environments.

## Overview

Docker automates application deployment within isolated containers that can run consistently on Linux, Windows, and macOS systems. The platform consists of both free and commercial offerings, with the core runtime called Docker Engine managing container operations.

## Historical Development

Solomon Hykes initiated the Docker project in 2013 as an internal initiative within dotCloud, a platform-as-a-service company founded during Y Combinator's Summer 2010 cohort. The company pivoted to become Docker, Inc. after the public debut at PyCon in Santa Clara that same year.

### Key Milestones

- **March 2013**: Released as open-source software
- **Version 0.9**: Replaced LXC with libcontainer, a Go-based component providing direct access to Linux kernel virtualization features
- **2017**: Creation of the Moby project for collaborative development
- **2019**: Windows Subsystem for Linux 2 support introduced, expanding Docker availability to Windows 10 Home
- **2021**: Docker Desktop licensing changed, restricting free use for enterprise customers

## Technical Architecture

### Core Components

**Docker Engine** comprises three functional elements:

1. **Daemon (dockerd)**: A persistent service managing containers and responding to Docker Engine API requests
2. **Client (docker)**: Command-line interface enabling user interaction with daemon processes
3. **Objects**: Images (read-only templates), containers (standardized execution environments), and services (scaled deployments across multiple daemons)

### Container Isolation

Containers isolate from each other while bundling software, libraries, and configuration files. They communicate through defined channels and consume fewer resources than virtual machines because all containers share a single operating system kernel.

On Linux systems, Docker utilizes kernel namespaces for process isolation and cgroups for resource constraints. Union-capable filesystems like OverlayFS enable multiple containers to run within a single Linux instance. macOS implementations run containers within a Linux virtual machine.

### Registry System

Docker registries serve as repositories for container images. Docker Hub functions as the default public registry, though private registries are supported. Registries enable image distribution through pull and push operations.

## Key Tools and Features

**Docker Compose**: Defines and orchestrates multi-container applications using YAML configuration files. Version 0.0.1 launched December 2013; version 1.0 became production-ready October 2014.

**Docker Swarm**: Provides native clustering, converting multiple Docker engines into a single virtual engine. Swarm mode integrated with Docker Engine starting with version 1.12, using the Raft consensus algorithm for cluster coordination.

**Docker Volume**: Enables persistent data storage independent of container lifecycle.

**Dockerfile**: Text-based specification defining container configuration, including base OS, runtime installations, environment variables, and port exposure.

## Adoption Timeline

Major technology companies embraced Docker:
- Red Hat (2013)
- Microsoft integration with Windows Server (2014)
- Amazon EC2 container services (2014)
- IBM strategic partnership (2014)
- Oracle Cloud support (2015)

By 2016, primary contributors included Docker team, Cisco, Google, Huawei, IBM, Microsoft, and Red Hat. LinkedIn data showed 160% Docker adoption growth during 2016.

## Licensing

Docker Engine operates under Apache License 2.0 for Linux deployments. Docker Desktop distributes components licensed under GNU General Public License, with commercial licensing required for large enterprises. The platform supports multiple processor architectures: x86-64, ARM, s390x, and ppc64le.

## Deployment Capabilities

Docker packages applications with dependencies in virtual containers deployable across on-premises, public cloud, and private cloud environments. A 2018 analysis found typical deployments run eight containers per host, with approximately 25% of organizations operating 18+ containers per host.

Docker supports installation on single-board computers like Raspberry Pi, extending containerization capabilities to embedded systems.
