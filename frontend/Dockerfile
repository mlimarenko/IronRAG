FROM node:22-bookworm-slim
WORKDIR /app

COPY package.json package-lock.json* ./
RUN npm ci --ignore-scripts \
  && node ./node_modules/esbuild/install.js \
  && npm rebuild esbuild

COPY . .

EXPOSE 3000
CMD ["npm", "run", "dev", "--", "--host", "0.0.0.0", "--port", "3000"]
