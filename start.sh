docker stop $(docker ps -q --filter ancestor=duck_summarizer) || true
docker build -t duck_summarizer .

docker run -d duck_summarizer# Build the Docker image