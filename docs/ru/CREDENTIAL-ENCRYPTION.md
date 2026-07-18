<div align="center">

# Эксплуатация шифрования credential

### Безопасное первое обновление, ввод write gate и ротация master key

[Обзор](./README.md) | [English](../en/CREDENTIAL-ENCRYPTION.md) | [Webhooks](./WEBHOOK.md) | [AI bindings](./AI-BINDINGS.md)

</div>

## Модель безопасности

IronRAG хранит credential AI-аккаунтов, signing secret исходящих webhook и
значения custom headers в аутентифицированных row-bound конвертах
`ironrag:enc:v3`. Master key должен храниться вне PostgreSQL и резервироваться
отдельно.

Два параметра решают разные задачи:

- `IRONRAG_CREDENTIAL_MASTER_KEY` и keyring позволяют текущей версии читать
  зашифрованные значения.
- `IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED` разрешает создавать и обновлять
  зашифрованные значения. По умолчанию он равен `false`, чтобы mixed-version
  deployment не записал ciphertext, который старый процесс примет за plaintext.

Если gate выключен или active key недоступен, операции записи credential
закрываются fail-closed. Чтение остаётся совместимым с plaintext, legacy
envelope, активным v3 key и настроенными previous v3 keys. Неизвестный v3 key ID
приводит к fail-closed ошибке.

`install.sh` включает зашифрованную запись только при создании новой `.env`.
Docker Compose передаёт значение, но по умолчанию использует `false`; Helm также
по умолчанию использует `false`.

## Первое обновление существующей установки

Не объединяйте deployment dual-reader, включение зашифрованной записи и
миграцию данных в один rolling update.

### 1. Подготовьте ключ и разверните версию с выключенной записью

1. Сделайте backup PostgreSQL и проверьте процедуру восстановления.
2. Сгенерируйте 32 случайных байта в canonical standard base64, например
   `openssl rand -base64 32 | tr -d '\n'`.
3. Сохраните ключ в secret manager отдельно от пароля БД и backup БД.
4. Передайте active key и key ID каждому API, worker и startup процессу. Оставьте
   `IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED=false`.
5. Разверните dual-reader версию. На этой фазе не создавайте, не ротируйте и не
   изменяйте credential provider или webhook.

Пока идёт rollout, старые процессы могут продолжать записывать plaintext, а
dual-reader его прочитает. Новая версия ещё не может записывать v3 envelope,
поэтому старый процесс не получит нечитаемый ciphertext.

### 2. Проверьте каждый API и worker

Проверьте **каждый replica**, а не один ответ через load balancer:

- убедитесь, что каждый API и worker pod/container использует нужный immutable
  image digest;
- напрямую запросите `/v1/version` у каждого replica и проверьте version и
  service role;
- дождитесь readiness каждого replica и проверьте startup logs на ошибки
  валидации keyring.

В Kubernetes проверьте image ID всех runtime pods и делайте port-forward к
каждому pod при запросе `/v1/version`:

```bash
kubectl get pods -l app.kubernetes.io/name=ironrag \
  -o custom-columns='NAME:.metadata.name,ROLE:.metadata.labels.app\.kubernetes\.io/component,IMAGE_ID:.status.containerStatuses[0].imageID,READY:.status.containerStatuses[0].ready'

# Повторите для каждого API и worker pod.
kubectl port-forward pod/<pod-name> 18080:8080
curl --fail --silent http://127.0.0.1:18080/v1/version
```

### 3. Включите запись отдельным rollout

Установите `IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED=true`, запустите второй
rollout и снова дождитесь перезапуска и readiness каждого API и worker. Только
после этого возобновляйте изменения credential.

После этой точки не откатывайтесь на версию без dual-reader. Если rollback
необходим, откатывайтесь только на версию, читающую v3 envelope, и сохраняйте
доступными все необходимые ключи.

### 4. Выполните inventory и миграцию

Сначала запустите dry inventory, затем apply, затем снова inventory:

```bash
docker compose run --rm backend ironrag-maintenance migrate credential-secrets
docker compose run --rm backend ironrag-maintenance migrate credential-secrets --apply
docker compose run --rm backend ironrag-maintenance migrate credential-secrets
```

Inventory аутентифицирует каждое зашифрованное значение, включая значения с уже
активным key ID, и повторно валидирует сохранённые webhook headers. Повреждённая
строка учитывается в ограниченной диагностике `(storage, id, error_code)`, а
сканирование продолжается; значения secret, URL, headers, nonce и ciphertext в
отчёт не попадают. Если остаётся хотя бы одно invalid-значение, команда завершится
с ненулевым кодом. Исправьте или ротируйте такие credential и повторяйте dry run,
пока счётчики invalid и rewrap не станут нулевыми.

Не удаляйте legacy reader и ключи, пока итоговый inventory или сохранённые backup
БД всё ещё от них зависят.

## Поведение Helm rollout

Для ConfigMap и Secret, которые рендерит chart, checksum-аннотации автоматически
перезапускают API, worker и startup pods при изменении runtime environment.

Helm не может посчитать checksum Secret, указанного через
`runtimeSecret.existingSecret`, поэтому вместе с ним обязателен непустой
`runtimeSecret.restartNonce`. Меняйте nonce при каждом изменении внешнего Secret.
Повторное использование nonce оставит запущенные pods со старыми env-значениями,
даже если объект Secret уже изменился.

```yaml
runtimeSecret:
  existingSecret: ironrag-runtime-production
  restartNonce: "credential-rollout-phase-1-2026-07-10"

app:
  credentialEncryptionWriteEnabled: false
```

Используйте новый nonce для write-on rollout и каждой фазы ротации ключа.

## Трёхфазная ротация master key

Key ID содержит 1-32 строчные латинские буквы, цифры, `.`, `_` или `-` и
начинается с буквы или цифры. `IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS`
поддерживает до восьми уникальных записей `id=canonical-base64-key` без пробелов,
строго отсортированных по key ID. Previous entry используется только для
decrypt/rewrap.

Пусть `key-2026-01` — активный ключ, а `key-2026-07` — новый.

### Фаза 1: раздайте новый ключ без переключения active

Оставьте старый ключ active. Добавьте новый в previous-key map и разверните этот
bridging keyring на каждом API и worker:

```env
IRONRAG_CREDENTIAL_MASTER_KEY_ID=key-2026-01
IRONRAG_CREDENTIAL_MASTER_KEY=<old-key>
IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS=key-2026-07=<new-key>
```

До продолжения убедитесь, что каждый replica успешно перезапущен. Несмотря на
название `previous`, размещение нового ключа здесь делает следующий rolling
overlap безопасным.

### Фаза 2: переключите active key

Сделайте новый ключ active, а старый оставьте как previous:

```env
IRONRAG_CREDENTIAL_MASTER_KEY_ID=key-2026-07
IRONRAG_CREDENTIAL_MASTER_KEY=<new-key>
IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS=key-2026-01=<old-key>
```

Во время rollout replica со старым config расшифровывают записи новым ключом,
потому что получили его на фазе 1. Replica с новым config расшифровывают записи
старым ключом через previous-key map.

### Фаза 3: rewrap, проверка и удаление старого ключа

Выполните последовательность dry/apply/dry. Удаляйте старый ключ только когда:

- итоговый inventory не содержит plaintext, legacy или old-key строк;
- каждый API и worker использует новый active key;
- backup БД с old-key envelope истёк согласно retention policy; и
- restore drill подтвердил соответствие сохранённых backup и key material.

После этого удалите старую запись, разверните сокращённый keyring на каждом
replica и снова проверьте readiness. Не уничтожайте единственную копию старого
ключа сразу: храните её согласно утверждённой политике retention и уничтожения.
