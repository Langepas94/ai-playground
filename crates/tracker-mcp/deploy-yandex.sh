#!/bin/bash
set -e

# Deploy tracker-mcp-http to Yandex Cloud Serverless Containers
# Prerequisites: yc CLI installed, Yandex account with Compute Cloud enabled

echo "=== Yandex Cloud Serverless Containers Deployment ==="
echo

# Step 1: Check yc CLI
if ! command -v yc &> /dev/null; then
    echo "❌ yc CLI not found. Install: https://cloud.yandex.com/en/docs/cli/operations/install-cli"
    exit 1
fi

# Step 2: Authenticate
echo "📌 Step 1: Authenticate with Yandex Cloud"
echo "Run: yc auth login"
echo "Then copy the link, authenticate in browser, copy token back to terminal."
read -p "Press Enter when authenticated..."

# Step 3: Set Yandex Cloud params
read -p "Enter Yandex Cloud Folder ID (find in console.cloud.yandex.com/folders): " FOLDER_ID
read -p "Enter Container Registry name (e.g., tracker-mcp): " REGISTRY_NAME
read -p "Enter Serverless Container name (e.g., tracker-mcp-prod): " CONTAINER_NAME

# Step 4: Configure yc defaults
echo "🔧 Setting yc defaults..."
yc config set folder-id "$FOLDER_ID"

REGISTRY_FULL="cr.yandex/$FOLDER_ID/$REGISTRY_NAME"
IMAGE_TAG="latest"
IMAGE_FULL="$REGISTRY_FULL:$IMAGE_TAG"

echo "Registry: $REGISTRY_FULL"
echo "Image: $IMAGE_FULL"
echo

# Step 5: Build Docker image locally
echo "📦 Step 2: Build Docker image (this takes ~3-5 min, first time)..."
docker build -t "$IMAGE_FULL" .

# Step 6: Get auth token for registry push
echo "🔐 Step 3: Authenticate Docker with Yandex Container Registry..."
yc container registry configure-docker

# Step 7: Push image
echo "📤 Pushing image to Yandex Container Registry..."
docker push "$IMAGE_FULL"

# Step 8: Create Serverless Container
echo "⚙️  Step 4: Creating Serverless Container..."
echo

# Generate MCP auth token
MCP_AUTH_TOKEN=$(openssl rand -hex 32)
echo "Generated MCP_AUTH_TOKEN: $MCP_AUTH_TOKEN"
echo "Save this token — you'll need it in ai-playground MCP config."
echo

# Read Tracker credentials
read -p "Enter TRACKER_TOKEN: " TRACKER_TOKEN
read -p "Enter TRACKER_ORG_ID: " TRACKER_ORG_ID

# Create container (stateless mode enabled for serverless)
yc serverless container create \
    --name "$CONTAINER_NAME" \
    --memory 512 \
    --cores 1 \
    --execution-timeout 30s \
    --environment "TRACKER_TOKEN=$TRACKER_TOKEN,TRACKER_ORG_ID=$TRACKER_ORG_ID,MCP_AUTH_TOKEN=$MCP_AUTH_TOKEN,MCP_STATELESS=1"

echo "✅ Container created: $CONTAINER_NAME"
echo

# Step 9: Create revision
echo "🚀 Step 5: Creating revision (first deployment)..."
yc serverless container revision deploy \
    --container-name "$CONTAINER_NAME" \
    --image "$IMAGE_FULL" \
    --cores 1 \
    --memory 512 \
    --execution-timeout 30s

echo "✅ Revision deployed"
echo

# Step 10: Get public endpoint
echo "🌐 Step 6: Retrieving public endpoint..."
CONTAINER_ID=$(yc serverless container get --name "$CONTAINER_NAME" --format json | jq -r '.id')
INVOKE_URL="https://$CONTAINER_ID.serverless.yandexcloud.net"

echo "✅ Deployment complete!"
echo
echo "=== MCP Endpoint ==="
echo "URL: $INVOKE_URL/mcp"
echo "Auth: Bearer $MCP_AUTH_TOKEN"
echo
echo "Add to ai-playground:"
echo "  - URL: $INVOKE_URL/mcp"
echo "  - Method: POST (MCP Streamable HTTP)"
echo "  - Header: Authorization: Bearer $MCP_AUTH_TOKEN"
echo
echo "Logs: yc serverless container logs --container-name $CONTAINER_NAME --tail 100"
echo "Update env: yc serverless container revision deploy --container-name $CONTAINER_NAME --update-environment-variables KEY=VALUE"
