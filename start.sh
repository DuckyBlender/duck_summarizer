#!/bin/bash

# Build the Docker image
echo "Building Docker image duck_summarizer..."
docker build -t duck_summarizer .

# Find and stop any previous containers running the duck_summarizer image
echo "Stopping any previous duck_summarizer containers..."
PREV_CONTAINER=$(docker ps -q --filter ancestor=duck_summarizer)

if [ -n "$PREV_CONTAINER" ]; then
  echo "Found previous container: $PREV_CONTAINER"
  docker stop $PREV_CONTAINER
  echo "Previous container stopped"
else
  echo "No previous container found"
fi

# Run a new container in detached mode
echo "Starting new duck_summarizer container..."
docker run -d duck_summarizer
echo "Container started"