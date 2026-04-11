module.exports = {
  apps: [
    {
      name: 'codex-gateway',
      script: './target/release/codex-gateway',
      cwd: '/root/dev/yow/gpt-gateway',
      env: {
        RUST_LOG: 'info'
      },
      autorestart: true,
      watch: false,
      max_restarts: 10,
      restart_delay: 2000
    }
  ]
};
