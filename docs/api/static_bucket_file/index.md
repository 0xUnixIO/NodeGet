# Static Bucket File 静态文件服务（文件操作）

`static-bucket-file` 命名空间负责操作**某个 Bucket 内的具体文件**，包括上传、读取、删除、重命名和列出文件。

::: info 与 Bucket 配置分离
本命名空间**不修改 Bucket 配置**（如 `is_http_root`、`cors`）。Bucket 配置的 CRUD 操作在 [
`static-bucket`](../static_bucket/index.md) 命名空间中完成。
:::

## 权限结构

- **Scope**：`StaticBucket(name)` 或 `Global`
    - `StaticBucket(name)`：只对该 bucket 生效
    - `Global`：对所有 bucket 生效（可用 `*` 通配）
- **Permission**：
    - `StaticBucketFile::Read` — 读取文件内容 (`read`)
    - `StaticBucketFile::Write` — 上传文件 (`upload`)
    - `StaticBucketFile::Delete` — 删除文件 (`delete`)
    - `StaticBucketFile::List` — 列出文件 (`list`)

::: tip 为什么 List 是独立权限？
文件列表操作只暴露目录结构（文件名、大小、修改时间），不暴露文件内容。将 `List` 与 `Read` 分离，可以授予"看得见目录"但不"
读得了内容"的权限粒度。
:::

## 方法概览

| 方法名                                           | 功能              | 所需权限                                                   |
|-----------------------------------------------|-----------------|--------------------------------------------------------|
| [static-bucket-file_upload](./crud.md#upload) | 上传文件            | `StaticBucketFile::Write`                              |
| [static-bucket-file_read](./crud.md#read)     | 读取文件（base64 返回） | `StaticBucketFile::Read`                               |
| [static-bucket-file_delete](./crud.md#delete) | 删除文件            | `StaticBucketFile::Delete`                             |
| [static-bucket-file_rename](./crud.md#rename) | 重命名 / 移动文件      | `StaticBucketFile::Write` + `StaticBucketFile::Delete` |
| [static-bucket-file_list](./crud.md#list)     | 列出目录下所有文件       | `StaticBucketFile::List`                               |

## HTTP 访问静态文件

### 默认路由

```
GET /nodeget/static/{name}/{*path}
```

访问示例：

```bash
curl http://localhost:3000/nodeget/static/my-site/index.html
```

当 `cors: true` 时，响应头会携带：

```
Access-Control-Allow-Origin: *
```

同时支持 `OPTIONS` 预检请求，返回 `204 No Content` 和对应的 CORS 头。

### 根路由代理（is_http_root）

当某条 Bucket 配置的 `is_http_root` 为 `true` 时：

- 服务器的根路由 `/` 和所有未匹配路由会直接返回该静态目录下的文件
- `WebSocket`（`/terminal`）和 `JSON-RPC` 不受影响
- 未绑定 `is_http_root` 时，根路由继续返回默认的占位 HTML

示例：

```bash
curl http://localhost:3000/index.html
# 实际返回磁盘上 {static_path}/{path}/index.html 的内容（path 为该 bucket 记录里的 path 字段）
```

## WebDAV 访问

除了 JSON-RPC，`static-bucket-file` 还支持标准的 **WebDAV 协议**，可直接挂载为本地网络盘（Windows / macOS / Linux 均支持）。

### 路由

```
/nodeget/static-webdav/{name}/**
```

### 鉴权

- **HTTP Basic Auth**
- **Username**：Token key（如 `0SbSt9j9NM8Tp1iu`）或 username
- **Password**：Token secret（如 `KXLOGPNgfDMtFHGLAaIyIGtbyHYVa53V`）或 password
- **权限要求**：必须同时拥有 `StaticBucketFile::Read` + `Write` + `Delete` + `List`

### 客户端挂载示例

**macOS**（Finder）：

```
Cmd + K → http://服务器IP:端口/nodeget/static-webdav/hugo
```

**Windows**（资源管理器）：

```
映射网络驱动器 → \\服务器IP@端口\nodeget\static-webdav\hugo
# 或使用 Explorer 地址栏：
http://服务器IP:端口/nodeget/static-webdav/hugo
```

**Linux**（命令行）：

```bash
# 使用 davfs2
sudo mount -t davfs http://服务器IP:端口/nodeget/static-webdav/hugo /mnt/hugo
# 或 GNOME Files (Nautilus)：Ctrl + L → dav://服务器IP:端口/nodeget/static-webdav/hugo
```

**命令行 curl 示例**：

```bash
# PROPFIND（列出目录）
curl -X PROPFIND http://localhost:3000/nodeget/static-webdav/hugo/ \
  -H "Authorization: Basic $(echo -n 'key:secret' | base64)" \
  -H "Content-Type: text/xml"

# PUT（上传文件）
curl -X PUT http://localhost:3000/nodeget/static-webdav/hugo/test.txt \
  -H "Authorization: Basic $(echo -n 'key:secret' | base64)" \
  -d "hello webdav"

# DELETE（删除文件）
curl -X DELETE http://localhost:3000/nodeget/static-webdav/hugo/test.txt \
  -H "Authorization: Basic $(echo -n 'key:secret' | base64)"
```

### 创建 WebDAV Token

任何拥有 `token_create` 权限的用户（包括 SuperToken）都可以创建：

```json
{
  "jsonrpc": "2.0",
  "method": "token_create",
  "params": [
    "t2DsqekSlWgR490E:mgD3tXOqkEGreGOZkweuwBaEEsGGbkmY",
    {
      "token_limit": [
        {
          "scopes": [{ "static_bucket": "hugo" }],
          "permissions": [
            { "static_bucket_file": "read" },
            { "static_bucket_file": "write" },
            { "static_bucket_file": "delete" },
            { "static_bucket_file": "list" }
          ]
        }
      ]
    }
  ],
  "id": 1
}
```

## 注意事项

- 文件 `path` 参数严格禁止目录遍历（`..` 和绝对路径会被拒绝）
- `body` 与 `base64` 不能同时出现，否则返回 `InvalidInput`
- 所有文件路径都经过 `resolve_safe_file_path` 双重校验，拒绝 `..` 穿透、绝对路径、反斜杠、Windows 盘符等
