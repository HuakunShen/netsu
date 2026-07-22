FROM mcr.microsoft.com/playwright:v1.61.1-noble

WORKDIR /app
RUN apt-get update \
  && apt-get install -y --no-install-recommends iptables \
  && rm -rf /var/lib/apt/lists/*
COPY package.json package-lock.json ./
RUN npm ci
COPY . ./

CMD ["npx", "playwright", "test"]
