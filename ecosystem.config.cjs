module.exports = {
  apps: [
    {
      name: 'codex-gateway',
      script: './target/release/codex-gateway',
      cwd: '/root/dev/yow/gpt-gateway',
      env: {
        RUST_LOG: 'info',
        ANTIGRAVITY_GOOGLE_CLIENT_ID: process.env.ANTIGRAVITY_GOOGLE_CLIENT_ID,
        ANTIGRAVITY_GOOGLE_CLIENT_SECRET: process.env.ANTIGRAVITY_GOOGLE_CLIENT_SECRET
      },
      autorestart: true,
      watch: false,
      max_restarts: 10,
      restart_delay: 2000
    }
  ]
};
