FROM oven/bun:1.3.14-alpine

WORKDIR /app

COPY package.json bun.lock ./
RUN bun install --frozen-lockfile --production

COPY tsconfig.json ./
COPY src ./src

RUN mkdir -p /data && chown -R bun:bun /app /data
USER bun

ENV NODE_ENV=production \
    DESKLINK_ACCOUNT_ADDR=0.0.0.0 \
    DESKLINK_ACCOUNT_PORT=3412 \
    DESKLINK_ACCOUNT_DATABASE=/data/desklink-account.sqlite

EXPOSE 3412
VOLUME ["/data"]

CMD ["bun", "run", "src/main.ts"]
