#!/bin/bash
set -e

echo "=== Yandex Cloud Serverless Containers Deploy ==="
echo

# Step 1: Check yc CLI
if ! command -v yc &> /dev/null; then
    echo "❌ yc CLI not found. Install: curl https://storage.yandexcloud.net/yandexcloud-yc/install.sh | bash"
    exit 1
fi

# Step 2: Auth
echo "Step 1: Authenticate with Yandex Cloud"
yc config get folder-id > /dev/null 2>&1 || {
    echo "Run: yc auth login"
    yc auth login
}

FOLDER_ID=$(yc config get folder-id)
echo "✓ Authenticated. Folder: $FOLDER_ID"
echo

# Step 3: Get/create service account
echo "Step 2: Service account (needed to pull images)"
read -p "Service account name (e.g., container-deployer): " SA_NAME
SA_ID=$(yc iam service-account get --name "$SA_NAME" --format json 2>/dev/null | jq -r '.id // empty')

if [ -z "$SA_ID" ]; then
    echo "Creating service account $SA_NAME..."
    SA_ID=$(yc iam service-account create --name "$SA_NAME" --format json | jq -r '.id')
    echo "✓ Created: $SA_ID"
else
    echo "✓ Using existing: $SA_ID"
fi
echo

# Step 4: Registry
read -p "Container Registry name (e.g., tracker-mcp): " REGISTRY_NAME
REGISTRY_FULL="cr.yandex/$FOLDER_ID/$REGISTRY_NAME"
echo "Registry: $REGISTRY_FULL"
echo

# Step 5: Build & push
echo "Step 3: Build and push Docker image"
docker build -t "$REGISTRY_FULL:latest" .
yc container registry configure-docker
docker push "$REGISTRY_FULL:latest"
echo "✓ Pushed"
echo

# Step 6: Container
read -p "Container name (e.g., tracker-mcp-prod): " CONTAINER_NAME

# Check if exists
CONTAINER_ID=$(yc serverless container get --name "$CONTAINER_NAME" --format json 2>/dev/null | jq -r '.id // empty')

if [ -z "$CONTAINER_ID" ]; then
    echo "Creating container $CONTAINER_NAME..."
    yc serverless container create --name "$CONTAINER_NAME"
    echo "✓ Created"
else
    echo "✓ Using existing container"
fi
echo

# Step 7: Env vars
echo "Step 4: Tracker credentials"
read -p "TRACKER_TOKEN: " TRACKER_TOKEN
read -p "TRACKER_ORG_ID: " TRACKER_ORG_ID

MCP_AUTH_TOKEN=$(openssl rand -hex 32)
echo "Generated MCP_AUTH_TOKEN: $MCP_AUTH_TOKEN"
echo

# Step 8: Deploy revision
echo "Step 5: Deploy revision"
yc serverless container revision deploy \
    --container-name "$CONTAINER_NAME" \
    --image "$REGISTRY_FULL:latest" \
    --cores 1 \
    --memory 512MB \
    --execution-timeout 30s \
    --service-account-id "$SA_ID" \
    --environment "TRACKER_TOKEN=$TRACKER_TOKEN,TRACKER_ORG_ID=$TRACKER_ORG_ID,MCP_AUTH_TOKEN=$MCP_AUTH_TOKEN,MCP_STATELESS=1"

echo "✓ Deployed"
echo

# Step 9: Get URL
CONTAINER_ID=$(yc serverless container get --name "$CONTAINER_NAME" --format json | jq -r '.id')
INVOKE_URL="https://$CONTAINER_ID.serverless.yandexcloud.net"

echo "=== Done ==="
echo "URL: $INVOKE_URL/mcp"
echo "Auth: Bearer $MCP_AUTH_TOKEN"
echo
echo "Add to ai-playground MCP config:"
echo "  URL: $INVOKE_URL/mcp"
echo "  Header: Authorization: Bearer $MCP_AUTH_TOKEN"
echo
echo "Logs: yc serverless container logs --container-name $CONTAINER_NAME --tail 100"
