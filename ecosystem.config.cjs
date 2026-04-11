module.exports = {
  apps: [
    {
      name: 'gpt-gateway',
      script: './target/release/gpt-gateway',
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
