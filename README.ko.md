<div align="center">

![lport](docs/banner.png)

**Linux와 macOS의 리스닝 포트를, 각 서버를 실행한 폴더와 함께 보여줍니다.**

[![Release](https://img.shields.io/github/v/release/Changroro/lport?style=flat-square&color=F5B240)](https://github.com/Changroro/lport/releases)
[![License](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-blue?style=flat-square)](#요구-사항)
[![Built with](https://img.shields.io/badge/Built%20with-Rust-CE422B?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Zero deps](https://img.shields.io/badge/Dependencies-stdlib%20only-444?style=flat-square)](Cargo.toml)

[빠른 시작](#빠른-시작) · [사용법](#사용법) · [작동 방식](#작동-방식) · [Releases](https://github.com/Changroro/lport/releases)

[English](README.md) · **한국어**

</div>

---

## 소개

매일 하게 되는 두 가지 질문에 답하는 작고(~550 KB) 의존성 없는 Rust CLI입니다:

1. *`8080` 포트, 뭐가 쓰고 있지?*
2. *저 서버, 어느 폴더에서 `npm run dev` 했더라?*

`lport`는 리스닝 중인 각 서버 프로세스의 **작업 디렉터리**를 보여줍니다 — 포트가 어느 프로젝트 것인지 바로 알 수 있습니다. Docker compose 컨테이너는 프로젝트 단위로 묶여서, 형제 서비스들(`supabase_db_*`, `supabase_kong_*`, …)이 흩어진 목록이 아니라 하나의 블록으로 읽힙니다. 포트를 되찾고 싶을 때도 `lport kill`은 시그널만 쏘고 끝나지 않습니다 — **프로세스가 실제로 종료됐는지 검증**하고, 살아남으면 SIGKILL 승격을 제안하고, 슈퍼바이저가 즉시 되살리면 경고합니다.

### 왜 필요한가

`lsof -i`와 `ss -tlnp`는 어떤 PID가 포트를 잡고 있는지는 알려주지만, 그 PID가 어느 프로젝트에서 왔는지는 알려주지 않습니다 — Docker 컨테이너도 그저 불투명한 프로세스 하나로 취급합니다.

`lport`는 그 빠진 층을 더합니다: 각 행에 cwd(또는 compose 프로젝트의 작업 디렉터리)가 붙고, Docker 행은 `[ project ]` 헤더 아래로 묶여 스택 전체가 한 블록으로 읽힙니다.

## 빠른 시작

```bash
curl -sfL https://raw.githubusercontent.com/Changroro/lport/main/install.sh | sh
```

Rust 툴체인이 필요합니다(없으면 스크립트가 한 줄 설치법을 알려줍니다). 설치 스크립트를 다시 실행하면 디스크의 버전을 감지해 이미 최신이면 건너뛰고, `--force`를 주면 무조건 재설치합니다.

```bash
# 신규 설치 또는 자동 업그레이드
curl -sfL https://raw.githubusercontent.com/Changroro/lport/main/install.sh | sh

# 강제 재설치
curl -sfL https://raw.githubusercontent.com/Changroro/lport/main/install.sh | sh -s -- --force

# 또는 cargo로 직접
cargo install --git https://github.com/Changroro/lport
```

## 사용법

```bash
lport                    # 대시보드: 사용자 서버 + docker 컨테이너
lport --dev              # 전부 (시스템 데몬 포함)
lport 8080               # `lport info 8080` 단축
lport info 8080          # 단일 포트 상세 블록
lport info 8080 5432     # 여러 포트
lport kill 3000          # 3000 포트의 프로세스에 SIGTERM
lport kill -9 3000 8080  # 여러 포트에 SIGKILL
lport kill -y 3000       # [y/N] 프롬프트 생략 (비대화형)
sudo lport               # 다른 사용자 프로세스까지 완전한 가시성
```

### 대시보드

<p align="center">
  <img src="demo/dashboard.gif" alt="lport dashboard" width="780"/>
</p>

기본 뷰는 Docker 컨테이너를 `com.docker.compose.project` 라벨로 묶어(없으면 컨테이너 이름) 같은 compose 프로젝트의 형제 컨테이너들이 하나의 `[ project ]` 블록으로 읽히게 합니다. 로컬 행은 `JOB` 컬럼에 전체 작업 디렉터리를 보여주며 `$HOME`은 `~`로 줄입니다. 이 컬럼은 **절대 잘리지 않습니다** — 경로를 잃으면 이 도구의 존재 이유가 사라지니까요. 컬럼 너비는 한글·이모지의 터미널 표시 폭까지 계산하므로 `~/프로젝트/데모` 같은 경로에서도 표가 흐트러지지 않습니다.

### 상세 보기

<p align="center">
  <img src="demo/info.gif" alt="lport info" width="780"/>
</p>

`lport info PORT...`(또는 그냥 `lport PORT...`)는 요청한 포트로 먼저 필터링한 뒤 PID별 상태를 읽으므로 단일 포트 조회가 가볍습니다. 블록에는 바인드 주소(`ADDR` — 한 포트에서 여러 주소에 바인드된 프로세스는 콤마로 나열되어 `127.0.0.1, ::`는 loopback + 전체 인터페이스로 읽힘), 부모 프로세스(`PARENT` — 슈퍼바이저가 포트를 계속 되살릴 때 부모를 종료할 수 있게), 사용자, CPU, MEM, 스레드 수(Linux), 가동 시간, 작업 디렉터리, 전체 커맨드라인이 표시됩니다. Docker 포트라면 컨테이너 이름, 이미지, compose 작업 디렉터리, 실시간 `docker stats` CPU / MEM이 추가됩니다. 요청한 포트 중 리스너가 없는 포트는 개별적으로 보고됩니다(`port N: no listening process found.`).

### 포트 종료

<p align="center">
  <img src="demo/kill.gif" alt="lport kill" width="780"/>
</p>

`lport kill PORT [PORT ...]`는 기본으로 `SIGTERM`을 보내고, `-9` 또는 `--force`를 주면 `SIGKILL`을 보냅니다. 각 프로세스는 PID·이름·작업 디렉터리를 보여주는 `[y/N]` 프롬프트로 확인을 받으며, 스크립트 같은 비대화형 호출자는 `-y` / `--yes`로 생략할 수 있습니다. 여러 포트(또는 tcp+udp)를 점유한 한 PID는 시그널을 한 번만 받습니다.

시그널을 보냈다고 프로세스가 따르는 건 아니므로, `lport`는 PID가 실제로 사라질 때까지 최대 3초를 기다린 뒤에만 `killed`를 보고합니다:

- **SIGTERM에도 살아있다면?** 대화형 실행에서는 승격을 제안합니다: `Escalate to SIGKILL? [y/N]`. 비대화형에서는 `lport kill -9 PORT` 힌트를 출력하고 `1`로 종료합니다.
- **바로 되살아난다면?** 죽인 포트가 잠시 후 다시 리스닝을 시작하면, 슈퍼바이저(dev 서버, systemd)가 재시작했을 가능성이 높다고 경고합니다 — 부모를 종료하세요.
- **권한이 없다면?** `kill`의 진단 메시지를 그대로 보여주고, EPERM이면 `sudo lport kill PORT`를 안내합니다.

Docker 포트에는 시그널을 보내지 않습니다. 대화형 터미널에서는 대신 컨테이너를 중지할지 제안하며(`Stop the container? [y/N]`), 거절하면 정상 종료(`0`)입니다. 비대화형일 때 — 파이프로 연결됐거나 `-y`를 줬을 때 — 는 컨테이너를 부수는 부작용 대신 해당하는 `docker stop <name>` 명령을 출력하고 `1`로 종료합니다.

```
$ lport kill 5432
port tcp/5432 is owned by Docker container 'supabase_db_supabase-prod'. Stop the container? [y/N] y
stopped container 'supabase_db_supabase-prod' (tcp/5432).
```

### Exit codes

| 명령 | `0` | `1` | `2` |
| --- | --- | --- | --- |
| `lport` / `--dev` | 항상 (출력이 비어도) | — | 알 수 없는 인자 |
| `lport info` (또는 `lport PORT`) | 요청한 포트 전부 발견 | 요청한 포트 중 리스너 없는 것이 있음 | 인자 오류 |
| `lport kill` | 모든 대상의 종료 확인, 사용자가 스킵, 또는 대화형으로 거절한 컨테이너 | 리스너 없음 / 시그널 실패 / 생존 / 비대화형에서 그대로 둔 Docker 포트 | 인자 오류 |

Exit code는 파이프가 닫혀도 유지됩니다: `lport kill -y 3000 8080 | head -1`도 두 포트 모두 시그널을 보내고 정직하게 보고합니다.

## 작동 방식

**Linux**:

- `ss -tlnpH` / `ss -ulnpH`로 TCP/UDP 리스닝 소켓 수집 (`ss` 실패는 "포트 없음"이 아니라 명확한 에러로 보고)
- `/proc/<pid>/{cwd,cmdline,stat,exe}`를 직접 읽음 — 추가 프로세스 스폰 없음
- `ps -o pid=,pcpu=,rss=,nlwp=,etime=,user=` (배치 1회)로 CPU / MEM / 가동 시간 / 사용자
- kill 검증은 `/proc/<pid>/stat` 폴링 — 좀비는 이미 소켓을 놓았으므로 종료로 취급

**macOS**:

- `lsof -nP -iTCP -sTCP:LISTEN` / `lsof -nP -iUDP`로 TCP/UDP 리스닝 소켓 수집
- `lsof -a -p <pids> -d cwd` (배치 1회)로 각 프로세스의 작업 디렉터리
- `ps -o pid=,tty=,comm=` (1차)로 TTY + 실행파일 이름
- `ps -o pid=,command=` (2차)로 전체 커맨드라인
- `ps -o pid=,pcpu=,rss=,etime=,user=`로 CPU / MEM / 가동 시간 / 사용자
- kill 검증은 `ps -o stat=` 폴링 — `kill -0`은 좀비를 살아있다고 오판하므로 쓰지 않음

macOS의 BSD `ps`에는 `nlwp`(스레드 수)가 없어 `THREADS` 행은 Linux 전용입니다.

**공통**:

- `docker ps`로 컨테이너/이미지/compose 프로젝트 매핑 — `(proto, host-port)` 키에 리스너의 **bind 주소와 IP 스택**까지 대조하므로, 특정 호스트 IP(또는 한쪽 스택)에만 publish된 컨테이너가 같은 포트 번호의 무관한 리스너를 차지하지 않고, 듀얼스택 publish(`0.0.0.0` + `[::]`)는 한 행으로 합쳐집니다. 대시보드 그룹핑은 `com.docker.compose.project` 라벨이 담당하고, 라벨이 없으면 컨테이너 이름을 씁니다.
- `docker stats --no-stream <name>` (`info` 모드 전용)으로 컨테이너 CPU / MEM — 조회 사이에 컨테이너가 사라졌으면 1회 재시도

대시보드는 Linux에서 ~130 ms에 돕니다. macOS는 `/proc` 대신 `lsof` / `ps`를 호출해서 약간 느립니다. `info`와 `kill` 서브커맨드는 요청한 포트로 먼저 필터링한 뒤 상세 정보를 읽으므로 단일 포트 작업이 전체 시스템 비용을 지불하지 않습니다. Docker 컨테이너가 끼면 ~1초가 추가됩니다.

## 요구 사항

- Linux 또는 macOS
- `ps` — 모든 Unix에 기본 탑재
- Linux: `iproute2` (`ss`) — 사실상 모든 배포판에 기본 탑재
- macOS: `lsof` — 기본 설치됨
- 선택: 컨테이너 매핑용 `docker`
- 선택: 백그라운드 업데이트 확인용 `curl` (없으면 건너뜀)

## 제한 사항

- **Unix 전용.** Windows는 지원하지 않습니다.
- `sudo` 없이는 다른 사용자 소유 프로세스가 `?`로 표시되거나(Linux) 아예 숨겨집니다(macOS — `lsof`는 권한 없이 타 프로세스 상태를 읽을 수 없음). PID를 알 수 없는 리스너는 소켓당 한 행씩 표시됩니다 — 보이지 않는 두 소켓이 같은 프로세스인지 `lport`는 추측하지 않습니다.
- compose가 아닌 일반 `docker run` 컨테이너는 `WORKDIR: -`로 표시됩니다 — Docker가 CLI 실행 디렉터리를 기록하지 않기 때문입니다.
- `dockerd`에 `userland-proxy: false`가 설정되면 publish된 포트에 호스트 리스너가 없습니다(iptables가 라우팅). 이때 같은 포트를 점유한 무관한 로컬 프로세스가 컨테이너 행으로 표시될 수 있습니다.

## 라이선스

MIT — [LICENSE](LICENSE) 참조.
