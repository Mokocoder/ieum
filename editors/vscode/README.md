# 이음 VS Code 확장

`.이음` 파일의 문법 색칠, 오류 표시, 자동 완성, 정의로 이동, 실행을 지원합니다.

## 설치

```sh
cargo install --path .
cd editors/vscode
npm install
npx @vscode/vsce package
code --install-extension ieum-0.1.0.vsix
```

`ieum`이 `PATH`에 없으면 설정의 `ieum.server.path`에 경로를 적습니다.
