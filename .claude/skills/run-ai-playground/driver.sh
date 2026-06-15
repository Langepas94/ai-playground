#!/bin/bash
# AI Playground Web UI Driver
# Usage: ./driver.sh [command]
# Commands: start, test-api, screenshot, interact, stop

set -e

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../" && pwd)"
BINARY="$PROJECT_ROOT/target/debug/ai"
WEB_PID_FILE="/tmp/ai-web.pid"
WEB_LOG="/tmp/ai-web.log"
PORT=8787
ADDR="127.0.0.1:$PORT"
BASE_URL="http://$ADDR"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log() {
    echo -e "${GREEN}[driver]${NC} $1"
}

error() {
    echo -e "${RED}[error]${NC} $1" >&2
}

warn() {
    echo -e "${YELLOW}[warn]${NC} $1"
}

# Kill existing process on port if any
cleanup_port() {
    lsof -i ":$PORT" 2>/dev/null | grep -v COMMAND | awk '{print $2}' | xargs kill -9 2>/dev/null || true
}

# Start the web server
start_server() {
    log "Starting ai web server on $ADDR..."
    cleanup_port

    cd "$PROJECT_ROOT"

    # Ensure binary is built
    if [ ! -f "$BINARY" ]; then
        log "Building binary..."
        cargo build 2>&1 | tail -5
    fi

    # Start server in background
    "$BINARY" web --listen "$ADDR" > "$WEB_LOG" 2>&1 &
    echo $! > "$WEB_PID_FILE"

    # Wait for server to be ready
    log "Waiting for server to be ready..."
    local timeout=10
    while [ $timeout -gt 0 ]; do
        if curl -s "$BASE_URL/" > /dev/null 2>&1; then
            log "Server ready at $BASE_URL"
            return 0
        fi
        sleep 0.5
        ((timeout--))
    done

    error "Server failed to start. Logs:"
    cat "$WEB_LOG"
    return 1
}

# Stop the server
stop_server() {
    if [ -f "$WEB_PID_FILE" ]; then
        local pid=$(cat "$WEB_PID_FILE")
        if kill -0 "$pid" 2>/dev/null; then
            log "Stopping server (PID $pid)..."
            kill "$pid" 2>/dev/null || true
            rm "$WEB_PID_FILE"
        fi
    fi
    cleanup_port
    log "Server stopped"
}

# Test API endpoints
test_api() {
    log "Testing API endpoints..."

    # Test /api/providers
    log "GET /api/providers"
    curl -s "$BASE_URL/api/providers" | jq '.providers | length' | xargs echo "  Providers:"

    # Test /api/agents (if implemented)
    log "GET /api/agents"
    curl -s "$BASE_URL/api/agents" 2>/dev/null | jq . | head -5 || log "  (endpoint not available or no agents)"

    # Test token status
    log "POST /api/token/status"
    curl -s -X POST "$BASE_URL/api/token/status" \
        -H "Content-Type: application/json" \
        -d '{"provider":"OpenRouter"}' | jq . | head -10
}

# Take a screenshot with chromium-cli
screenshot() {
    local output_file="${1:-/tmp/ai-web-screenshot.png}"
    log "Taking screenshot to $output_file..."

    chromium-cli screenshot "$BASE_URL" --timeout 5 --output "$output_file"

    if [ -f "$output_file" ]; then
        log "Screenshot saved: $output_file"
    else
        error "Failed to take screenshot"
        return 1
    fi
}

# Interactive REPL for testing
interact() {
    log "Entering interactive mode. Commands:"
    echo "  screenshot [file]     - Take a screenshot"
    echo "  test-api              - Test API endpoints"
    echo "  curl [path]           - Raw curl request"
    echo "  open [path]           - Open URL in browser"
    echo "  exit/quit             - Exit"
    echo ""

    while true; do
        read -p "driver> " cmd rest
        case "$cmd" in
            screenshot)
                screenshot "$rest"
                ;;
            test-api)
                test_api
                ;;
            curl)
                curl -s "$BASE_URL$rest" | jq . 2>/dev/null || echo "Invalid JSON response"
                ;;
            open)
                log "Opening $BASE_URL$rest"
                open "$BASE_URL$rest" 2>/dev/null || xdg-open "$BASE_URL$rest" 2>/dev/null || echo "Unable to open browser"
                ;;
            exit|quit)
                break
                ;;
            *)
                if [ -n "$cmd" ]; then
                    echo "Unknown command: $cmd"
                fi
                ;;
        esac
    done
}

# Main
case "${1:-start}" in
    start)
        start_server
        ;;
    stop)
        stop_server
        ;;
    test-api)
        if [ ! -f "$WEB_PID_FILE" ] || ! kill -0 "$(cat "$WEB_PID_FILE")" 2>/dev/null; then
            error "Server not running. Start with: $0 start"
            exit 1
        fi
        test_api
        ;;
    screenshot)
        if [ ! -f "$WEB_PID_FILE" ] || ! kill -0 "$(cat "$WEB_PID_FILE")" 2>/dev/null; then
            error "Server not running. Start with: $0 start"
            exit 1
        fi
        screenshot "$2"
        ;;
    interact)
        if [ ! -f "$WEB_PID_FILE" ] || ! kill -0 "$(cat "$WEB_PID_FILE")" 2>/dev/null; then
            error "Server not running. Start with: $0 start"
            exit 1
        fi
        interact
        ;;
    *)
        echo "Usage: $0 {start|stop|test-api|screenshot|interact}"
        exit 1
        ;;
esac
