#!/usr/bin/env bash
# Deployment script for a containerized web application.
# Handles building, tagging, health checks, and rollback.

set -euo pipefail

readonly APP_NAME="webapp"
readonly REGISTRY="registry.example.com"
readonly HEALTH_ENDPOINT="/api/health"
readonly DEPLOY_TIMEOUT=120
readonly HEALTH_CHECK_INTERVAL=5

# Prints a timestamped log message to stderr.
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" >&2
}

# Builds the Docker image and tags it with the git SHA and 'latest'.
# Exits with an error if the build fails or if the working tree is dirty.
#
# Arguments:
#   $1 - Docker context directory (default: current directory)
# Outputs:
#   The full image tag that was built.
build_image() {
    local context_dir="${1:-.}"
    local git_sha
    git_sha=$(git rev-parse --short HEAD)

    if [[ -n "$(git status --porcelain)" ]]; then
        log "ERROR: Working tree is dirty. Commit or stash changes first."
        return 1
    fi

    local image_tag="${REGISTRY}/${APP_NAME}:${git_sha}"

    log "Building image ${image_tag} from ${context_dir}"
    docker build \
        --build-arg "BUILD_SHA=${git_sha}" \
        --build-arg "BUILD_TIME=$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        -t "${image_tag}" \
        -t "${REGISTRY}/${APP_NAME}:latest" \
        "${context_dir}"

    log "Pushing ${image_tag}"
    docker push "${image_tag}"
    docker push "${REGISTRY}/${APP_NAME}:latest"

    echo "${image_tag}"
}

# Deploys the given image tag to the target environment.
# Updates the Kubernetes deployment and waits for rollout to complete.
#
# Arguments:
#   $1 - Image tag to deploy
#   $2 - Target environment (staging or production)
# Returns:
#   0 on success, 1 on rollout timeout
deploy_to_environment() {
    local image_tag="$1"
    local environment="$2"

    if [[ "${environment}" != "staging" && "${environment}" != "production" ]]; then
        log "ERROR: Invalid environment '${environment}'. Must be 'staging' or 'production'."
        return 1
    fi

    local namespace="${APP_NAME}-${environment}"

    log "Deploying ${image_tag} to ${environment} (namespace: ${namespace})"

    kubectl set image \
        "deployment/${APP_NAME}" \
        "${APP_NAME}=${image_tag}" \
        --namespace="${namespace}"

    log "Waiting for rollout to complete (timeout: ${DEPLOY_TIMEOUT}s)"
    if ! kubectl rollout status \
        "deployment/${APP_NAME}" \
        --namespace="${namespace}" \
        --timeout="${DEPLOY_TIMEOUT}s"; then
        log "ERROR: Rollout timed out for ${environment}"
        return 1
    fi

    log "Deployment to ${environment} completed successfully"
    return 0
}

# Performs a health check against the deployed service.
# Retries with exponential backoff until the service responds
# with HTTP 200 or the timeout is exceeded.
#
# Arguments:
#   $1 - Base URL of the service
#   $2 - Maximum wait time in seconds (default: 60)
# Returns:
#   0 if healthy, 1 if timed out
wait_for_healthy() {
    local base_url="$1"
    local max_wait="${2:-60}"
    local elapsed=0
    local interval="${HEALTH_CHECK_INTERVAL}"

    log "Waiting for ${base_url}${HEALTH_ENDPOINT} to become healthy"

    while (( elapsed < max_wait )); do
        local status_code
        status_code=$(curl -s -o /dev/null -w '%{http_code}' \
            "${base_url}${HEALTH_ENDPOINT}" || echo "000")

        if [[ "${status_code}" == "200" ]]; then
            log "Health check passed after ${elapsed}s"
            return 0
        fi

        log "Health check returned ${status_code}, retrying in ${interval}s..."
        sleep "${interval}"
        elapsed=$(( elapsed + interval ))
    done

    log "ERROR: Health check timed out after ${max_wait}s"
    return 1
}

# Rolls back the deployment to the previous revision.
#
# Arguments:
#   $1 - Target environment
rollback() {
    local environment="$1"
    local namespace="${APP_NAME}-${environment}"

    log "Rolling back ${environment} to previous revision"
    kubectl rollout undo "deployment/${APP_NAME}" --namespace="${namespace}"
    kubectl rollout status "deployment/${APP_NAME}" --namespace="${namespace}" \
        --timeout="${DEPLOY_TIMEOUT}s"
    log "Rollback complete for ${environment}"
}

# Main entry point. Builds, deploys, and verifies the application.
main() {
    local environment="${1:-staging}"
    local context_dir="${2:-.}"

    log "Starting deployment pipeline for ${APP_NAME} to ${environment}"

    local image_tag
    image_tag=$(build_image "${context_dir}")

    if ! deploy_to_environment "${image_tag}" "${environment}"; then
        log "Deployment failed, initiating rollback"
        rollback "${environment}"
        return 1
    fi

    local service_url
    service_url=$(kubectl get svc "${APP_NAME}" \
        --namespace="${APP_NAME}-${environment}" \
        -o jsonpath='{.status.loadBalancer.ingress[0].hostname}')

    if ! wait_for_healthy "https://${service_url}"; then
        log "Post-deploy health check failed, rolling back"
        rollback "${environment}"
        return 1
    fi

    log "Deployment pipeline completed successfully"
}

main "$@"
