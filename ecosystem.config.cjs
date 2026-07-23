module.exports = {
  apps: [
    {
      name: 'io-gateway',
      script: './target/release/io-gateway',
      cwd: '/root/dev/yow/io-gateway',
      env: {
        RUST_LOG: 'info',
        ANTIGRAVITY_GOOGLE_CLIENT_ID: process.env.ANTIGRAVITY_GOOGLE_CLIENT_ID,
        ANTIGRAVITY_GOOGLE_CLIENT_SECRET: process.env.ANTIGRAVITY_GOOGLE_CLIENT_SECRET,
        GEMINI_GOOGLE_CLIENT_ID: process.env.GEMINI_GOOGLE_CLIENT_ID,
        GEMINI_GOOGLE_CLIENT_SECRET: process.env.GEMINI_GOOGLE_CLIENT_SECRET
      },
      autorestart: true,
      watch: false,
      max_restarts: 10,
      restart_delay: 2000
    }
  ]
};
