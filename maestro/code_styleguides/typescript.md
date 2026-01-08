# TypeScript Style Guide

A comprehensive guide for writing clean, maintainable, and type-safe TypeScript code. This guide combines industry best practices from Google's TypeScript Style Guide, Microsoft's official recommendations, and modern TypeScript development patterns.

## Table of Contents

- [Code Formatting](#code-formatting)
- [Naming Conventions](#naming-conventions)
- [Type System Best Practices](#type-system-best-practices)
- [Functions and Methods](#functions-and-methods)
- [Classes and Interfaces](#classes-and-interfaces)
- [Generics](#generics)
- [Error Handling](#error-handling)
- [Module Organization](#module-organization)
- [Tooling and Configuration](#tooling-and-configuration)
- [Common Patterns](#common-patterns)
- [Anti-Patterns to Avoid](#anti-patterns-to-avoid)

---

## Code Formatting

### Indentation and Spacing

```typescript
// Use 2 spaces for indentation
function processData(data: UserData): ProcessedData {
  if (!data) {
    return null;
  }
  // Function body with 2-space indent
}

// Use spaces around operators
const result = value1 + value2;
const isActive = status === 'active';

// Space after colon in type annotations
function greet(name: string): void {
  console.log(`Hello, ${name}`);
}
```

### Line Length

- Maximum line length: **100 characters** (soft limit: 80 for readability)
- Break long lines at logical points

```typescript
// Good: Logical line breaks
interface UserResponse {
  id: string;
  email: string;
  profile: UserProfile;
  preferences: UserPreferences;
}

// Avoid: Excessively long lines
interface UserResponse { id: string; email: string; profile: UserProfile; preferences: UserPreferences; metadata: Record<string, unknown> }
```

### Semicolons

**Always use semicolons** to terminate statements. Never rely on Automatic Semicolon Insertion (ASI).

```typescript
// Good
const value = 42;
const doubled = value * 2;

// Bad: Relying on ASI
const value = 42
const doubled = value * 2
```

### Quotes

- Use **single quotes** (`'`) for string literals
- Use **template literals** (backticks) for interpolation or multi-line strings

```typescript
// Good
const message = 'Hello, World!';
const greeting = `Hello, ${userName}!`;

// Bad
const message = "Hello, World!";
const greeting = 'Hello, ' + userName + '!';
```

---

## Naming Conventions

### General Rules

| Entity | Convention | Example |
|--------|-----------|---------|
| Classes, Interfaces, Types, Enums | `PascalCase` | `UserService`, `UserProfile` |
| Variables, Functions, Methods | `camelCase` | `getUserData`, `isActive` |
| Constants | `UPPER_SNAKE_CASE` | `MAX_RETRY_COUNT`, `API_BASE_URL` |
| Private Properties | `camelCase` (no underscore prefix) | `private internalState` |
| Type Parameters | `PascalCase` with descriptive name | `TEntityType`, `TOptions` |
| File Names | `camelCase` or `kebab-case` | `userService.ts`, `user-service.ts` |

### Interface Naming

```typescript
// Good: Descriptive interface names
interface UserRepository {
  findById(id: string): Promise<User | null>;
  create(user: CreateUserDto): Promise<User>;
}

// Good: Prefix interfaces with 'I' ONLY in legacy codebases
interface IUserService {
  // Avoid this in new code
}

// Good: Use descriptive names for clarity
interface UserSearchParams {
  email?: string;
  name?: string;
  minAge?: number;
}
```

### Enum Naming

```typescript
// Good: PascalCase for enum, UPPER_CASE for values
enum UserRole {
  ADMIN = 'admin',
  USER = 'user',
  GUEST = 'guest',
}

// Good: String enums for type safety
enum HttpStatus {
  OK = '200',
  NOT_FOUND = '404',
  SERVER_ERROR = '500',
}
```

### Function Naming

```typescript
// Good: Verb-based function names
function getUserById(id: string): Promise<User> {
  return database.query('SELECT * FROM users WHERE id = ?', [id]);
}

function validateEmail(email: string): boolean {
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email);
}

// Good: Prefix boolean-returning functions with 'is', 'has', 'should', 'can'
function isActiveUser(user: User): boolean {
  return user.status === 'active';
}

function hasPermission(user: User, permission: string): boolean {
  return user.permissions.includes(permission);
}
```

---

## Type System Best Practices

### Type Inference vs Explicit Types

```typescript
// Good: Use inference for obvious types
const count = 0;
const name = 'John';
const users = [];
const isActive = true;

// Good: Be explicit for complex types or return values
interface User {
  id: string;
  name: string;
  email: string;
}

function createUser(userData: Partial<User>): User {
  return {
    id: generateId(),
    name: userData.name || '',
    email: userData.email || '',
  };
}

// Good: Explicit return types for public APIs
async function fetchUser(id: string): Promise<User | null> {
  const response = await fetch(`/api/users/${id}`);
  if (!response.ok) return null;
  return response.json();
}
```

### Avoid `any` Type

```typescript
// Bad: Using 'any' loses type safety
function processValue(value: any): any {
  return value * 2;
}

// Good: Use 'unknown' for truly unknown types
function processValue(value: unknown): unknown {
  if (typeof value === 'number') {
    return value * 2;
  }
  throw new Error('Value must be a number');
}

// Good: Use generics for type-safe reusable code
function processValue<T extends number>(value: T): T {
  return value;
}

// Good: Use specific types
type StringOrNumber = string | number;
function displayValue(value: StringOrNumber): void {
  console.log(value);
}
```

### Null and Undefined Handling

```typescript
// Good: Use optional properties (?:) instead of \| undefined
interface UserConfig {
  theme: 'light' | 'dark';
  language: string;
  notifications?: boolean; // Preferred
  // notifications: boolean | undefined; // Avoid
}

// Good: Use non-null assertion (!) sparingly and with justification
function getUserName(user: User | null): string {
  // Only use ! when you're certain the value is not null
  return user!.name; // Justification: We've validated this upstream
}

// Better: Use proper null checks
function getUserName(user: User | null): string {
  if (!user) {
    throw new Error('User is required');
  }
  return user.name;
}

// Good: Use nullish coalescing (??) for default values
function getTheme(theme?: string | null): string {
  return theme ?? 'default';
}

// Good: Use optional chaining (?.) for safe property access
const userEmail = user?.profile?.email ?? 'unknown@example.com';
```

### Type Guards and Discriminated Unions

```typescript
// Good: Use type guards for runtime type checking
function isString(value: unknown): value is string {
  return typeof value === 'string';
}

function processValue(value: unknown): string {
  if (isString(value)) {
    return value.toUpperCase(); // TypeScript knows this is a string
  }
  return String(value);
}

// Good: Use discriminated unions for type-safe state management
type RequestState =
  | { status: 'idle' }
  | { status: 'loading' }
  | { status: 'success'; data: UserData }
  | { status: 'error'; error: Error };

function handleRequest(state: RequestState): void {
  switch (state.status) {
    case 'idle':
      console.log('Request not started');
      break;
    case 'loading':
      console.log('Request in progress');
      break;
    case 'success':
      console.log('Data received:', state.data); // TypeScript knows data exists
      break;
    case 'error':
      console.error('Error occurred:', state.error.message);
      break;
  }
}
```

### Utility Types

```typescript
// Good: Use built-in utility types
interface User {
  id: string;
  name: string;
  email: string;
  password: string;
  createdAt: Date;
}

// Partial - Make all properties optional
type CreateUserDto = Partial<User>;

// Required - Make all properties required
type RequiredUser = Required<User>;

// Pick - Select specific properties
type UserSummary = Pick<User, 'id' | 'name'>;

// Omit - Exclude specific properties
type PublicUser = Omit<User, 'password'>;

// Record - Create object types with specific keys
type UserRoles = Record<string, UserRole>;

// ReturnType - Extract function return type
type GetUserReturn = ReturnType<typeof getUser>;

// Parameters - Extract function parameters
type GetUserParams = Parameters<typeof getUser>;

// Good: Combine utility types
type UpdateUserDto = Partial<Pick<User, 'name' | 'email'>>;
```

---

## Functions and Methods

### Function Declarations

```typescript
// Good: Function declarations for hoisted functions
function calculateTotal(items: CartItem[]): number {
  return items.reduce((sum, item) => sum + item.price, 0);
}

// Good: Arrow functions for callbacks and non-hoisted functions
const calculateTotal = (items: CartItem[]): number => {
  return items.reduce((sum, item) => sum + item.price, 0);
};

// Good: Concise arrow functions for simple operations
const calculateTotal = (items: CartItem[]): number =>
  items.reduce((sum, item) => sum + item.price, 0);
```

### Function Parameters

```typescript
// Good: Use object parameters for multiple related arguments
function createUser({
  name,
  email,
  age,
}: {
  name: string;
  email: string;
  age?: number;
}): User {
  return { id: generateId(), name, email, age };
}

// Good: Provide defaults for optional parameters
function greet(
  name: string,
  greeting: string = 'Hello',
  punctuation: string = '!'
): string {
  return `${greeting}, ${name}${punctuation}`;
}

// Good: Use rest parameters for variable arguments
function sum(...numbers: number[]): number {
  return numbers.reduce((total, n) => total + n, 0);
}

// Good: Destructure with type annotations
function processUser({ id, name, profile }: User): void {
  console.log(`Processing ${name} (${id})`);
}
```

### Async Functions

```typescript
// Good: Always return Promises from async functions
async function fetchUser(id: string): Promise<User> {
  const response = await fetch(`/api/users/${id}`);
  if (!response.ok) {
    throw new Error(`User not found: ${id}`);
  }
  return response.json();
}

// Good: Handle errors appropriately
async function getUserData(id: string): Promise<User | null> {
  try {
    return await fetchUser(id);
  } catch (error) {
    console.error('Failed to fetch user:', error);
    return null;
  }
}

// Good: Use Promise.all for parallel operations
async function fetchUsersWithPosts(ids: string[]): Promise<UserWithPosts[]> {
  const [users, posts] = await Promise.all([
    fetchUsers(ids),
    fetchPosts(ids),
  ]);

  return users.map(user => ({
    ...user,
    posts: posts.filter(post => post.userId === user.id),
  }));
}
```

---

## Classes and Interfaces

### Class Design

```typescript
// Good: Use classes for encapsulating behavior and state
class UserService {
  private readonly repository: UserRepository;
  private cache: Map<string, User> = new Map();

  constructor(repository: UserRepository) {
    this.repository = repository;
  }

  async findById(id: string): Promise<User | null> {
    // Check cache first
    if (this.cache.has(id)) {
      return this.cache.get(id)!;
    }

    // Fetch from repository
    const user = await this.repository.findById(id);
    if (user) {
      this.cache.set(id, user);
    }
    return user;
  }

  clearCache(): void {
    this.cache.clear();
  }
}

// Good: Use interfaces for contracts
interface IUserRepository {
  findById(id: string): Promise<User | null>;
  create(user: CreateUserDto): Promise<User>;
  update(id: string, data: UpdateUserDto): Promise<User>;
  delete(id: string): Promise<boolean>;
}

// Good: Prefer readonly for immutable properties
class User {
  readonly id: string;
  readonly createdAt: Date;
  name: string;
  email: string;

  constructor(id: string, name: string, email: string) {
    this.id = id;
    this.name = name;
    this.email = email;
    this.createdAt = new Date();
  }
}
```

### Access Modifiers

```typescript
// Good: Use appropriate access modifiers
class UserService {
  // Public by default - accessible everywhere
  public readonly version: string = '1.0.0';

  // Private - only accessible within this class
  private cache: Map<string, User> = new Map();

  // Protected - accessible in this class and subclasses
  protected logger: Logger;

  constructor(logger: Logger) {
    this.logger = logger;
  }

  // Public method
  async getUser(id: string): Promise<User> {
    return this.fetchUser(id);
  }

  // Private method - internal implementation
  private async fetchUser(id: string): Promise<User> {
    const cached = this.cache.get(id);
    if (cached) return cached;

    const user = await this.repository.findById(id);
    if (user) {
      this.cache.set(id, user);
    }
    return user;
  }

  // Protected method - can be overridden by subclasses
  protected logError(error: Error): void {
    this.logger.error(error.message);
  }
}
```

### Interfaces vs Type Aliases

```typescript
// Good: Use interfaces for object shapes that can be extended
interface User {
  id: string;
  name: string;
  email: string;
}

// Interfaces can be extended
interface AdminUser extends User {
  permissions: string[];
  role: 'admin';
}

// Good: Use type aliases for unions, intersections, and complex types
type UserRole = 'admin' | 'user' | 'guest';
type UserOrAdmin = User | AdminUser;
type UserWithRoles = User & { roles: string[] };

// Good: Use interfaces for public APIs
interface UserService {
  getUser(id: string): Promise<User>;
  createUser(data: CreateUserDto): Promise<User>;
  updateUser(id: string, data: UpdateUserDto): Promise<User>;
}

// Good: Use type aliases for internal or complex types
type UserMap = Map<string, User>;
type UserCallback = (user: User) => void;
type AsyncUserOperation = (id: string) => Promise<User>;
```

### Abstract Classes

```typescript
// Good: Use abstract classes for shared implementation
abstract class BaseRepository<T> {
  abstract findById(id: string): Promise<T | null>;
  abstract findAll(): Promise<T[]>;

  async exists(id: string): Promise<boolean> {
    const entity = await this.findById(id);
    return entity !== null;
  }

  async findAllOrEmpty(): Promise<T[]> {
    return this.findAll().catch(() => []);
  }
}

class UserRepository extends BaseRepository<User> {
  async findById(id: string): Promise<User | null> {
    // Implementation
  }

  async findAll(): Promise<User[]> {
    // Implementation
  }
}
```

---

## Generics

### Generic Functions

```typescript
// Good: Use generics for reusable, type-safe functions
function first<T>(array: T[]): T | undefined {
  return array[0];
}

function map<T, U>(array: T[], mapper: (item: T) => U): U[] {
  return array.map(mapper);
}

// Usage
const numbers = [1, 2, 3, 4, 5];
const firstNumber = first(numbers); // Type: number | undefined

const strings = map(numbers, n => n.toString()); // Type: string[]
```

### Generic Constraints

```typescript
// Good: Constrain generics with extends
function getProperty<T, K extends keyof T>(obj: T, key: K): T[K] {
  return obj[key];
}

function logLength<T extends { length: number }>(value: T): void {
  console.log(`Length: ${value.length}`);
}

// Usage
const user = { name: 'John', age: 30 };
const name = getProperty(user, 'name'); // Type: string
// const invalid = getProperty(user, 'invalid'); // Error!

logLength('hello'); // OK
logLength([1, 2, 3]); // OK
// logLength(42); // Error!
```

### Generic Classes

```typescript
// Good: Use generics for flexible, type-safe classes
class Repository<T extends { id: string }> {
  private items: Map<string, T> = new Map();

  save(item: T): void {
    this.items.set(item.id, item);
  }

  findById(id: string): T | undefined {
    return this.items.get(id);
  }

  findAll(): T[] {
    return Array.from(this.items.values());
  }
}

// Usage
interface User {
  id: string;
  name: string;
}

interface Product {
  id: string;
  price: number;
}

const userRepo = new Repository<User>();
const productRepo = new Repository<Product>();
```

### Default Type Parameters

```typescript
// Good: Provide sensible defaults for type parameters
class EventEmitter<EventMap extends Record<string, unknown> = Record<string, unknown>> {
  private listeners: Map<keyof EventMap, Function[]> = new Map();

  on<K extends keyof EventMap>(
    event: K,
    callback: (data: EventMap[K]) => void
  ): void {
    const existing = this.listeners.get(event) || [];
    this.listeners.set(event, [...existing, callback]);
  }

  emit<K extends keyof EventMap>(event: K, data: EventMap[K]): void {
    const callbacks = this.listeners.get(event) || [];
    callbacks.forEach(cb => cb(data));
  }
}

// Usage
interface UserEvents {
  'user:created': { id: string; name: string };
  'user:deleted': { id: string };
}

const emitter = new EventEmitter<UserEvents>();
emitter.on('user:created', (data) => {
  console.log(`User created: ${data.name}`);
});
```

---

## Error Handling

### Custom Error Classes

```typescript
// Good: Create custom error classes for different error types
class AppError extends Error {
  constructor(
    message: string,
    public readonly code: string,
    public readonly statusCode: number = 500,
    public readonly isOperational: boolean = true
  ) {
    super(message);
    this.name = this.constructor.name;
    Error.captureStackTrace(this, this.constructor);
  }
}

class ValidationError extends AppError {
  constructor(message: string, public readonly field: string) {
    super(message, 'VALIDATION_ERROR', 400);
  }
}

class NotFoundError extends AppError {
  constructor(resource: string, id: string) {
    super(`${resource} not found: ${id}`, 'NOT_FOUND', 404);
  }
}

// Usage
function validateEmail(email: string): void {
  if (!email.includes('@')) {
    throw new ValidationError('Invalid email format', 'email');
  }
}

async function getUser(id: string): Promise<User> {
  const user = await database.findUser(id);
  if (!user) {
    throw new NotFoundError('User', id);
  }
  return user;
}
```

### Error Handling Patterns

```typescript
// Good: Use try-catch for async operations
async function processUserData(userId: string): Promise<ProcessedData> {
  try {
    const user = await fetchUser(userId);
    const processed = await processData(user);
    return processed;
  } catch (error) {
    if (error instanceof NotFoundError) {
      logger.warn('User not found:', error.message);
      return getDefaultData();
    }
    if (error instanceof AppError) {
      logger.error('Application error:', error.message);
      throw error;
    }
    // Unknown error - wrap it
    throw new AppError(
      'Failed to process user data',
      'PROCESSING_ERROR',
      500
    );
  }
}

// Good: Use Result type for error handling without exceptions
type Result<T, E = Error> =
  | { success: true; data: T }
  | { success: false; error: E };

async function safeGetUser(id: string): Promise<Result<User>> {
  try {
    const user = await fetchUser(id);
    return { success: true, data: user };
  } catch (error) {
    return { success: false, error: error as Error };
  }
}

// Usage
const result = await safeGetUser(userId);
if (result.success) {
  console.log('User:', result.data);
} else {
  console.error('Error:', result.error.message);
}
```

---

## Module Organization

### File Structure

```typescript
// Good: Organize files by feature
src/
  features/
    auth/
      index.ts
      types.ts
      service.ts
      controller.ts
      repository.ts
      utils.ts
    users/
      index.ts
      types.ts
      service.ts
      controller.ts
      repository.ts
  shared/
    types/
    utils/
    constants/
  config/
    index.ts
```

### Export Strategies

```typescript
// Good: Use barrel exports (index.ts) for clean imports
// features/auth/index.ts
export * from './types';
export * from './service';
export * from './controller';

// Usage in other files
import { AuthService, AuthController, LoginCredentials } from '@/features/auth';

// Good: Export types separately from values
// userService.ts
export class UserService {
  // implementation
}

export type { User, CreateUserDto, UpdateUserDto };

// Good: Re-export commonly used items
// features/users/index.ts
export { UserService } from './service';
export type { User, CreateUserDto } from './types';
```

### Import Ordering

```typescript
// Good: Organize imports by group
// 1. Node.js built-ins
import path from 'path';
import fs from 'fs';

// 2. External dependencies
import express from 'express';
import lodash from 'lodash';

// 3. Internal modules
import { config } from '@/config';
import { UserService } from '@/features/users';

// 4. Types (if separate)
import type { User, CreateUserDto } from '@/types';

// 5. Relative imports
import { logger } from './utils/logger';
import { formatUser } from './formatters';
```

---

## Tooling and Configuration

### ESLint Configuration

```json
{
  "extends": [
    "eslint:recommended",
    "plugin:@typescript-eslint/recommended",
    "plugin:@typescript-eslint/recommended-requiring-type-checking",
    "prettier"
  ],
  "parser": "@typescript-eslint/parser",
  "parserOptions": {
    "ecmaVersion": 2022,
    "sourceType": "module",
    "project": "./tsconfig.json"
  },
  "rules": {
    "@typescript-eslint/no-unused-vars": "error",
    "@typescript-eslint/no-explicit-any": "warn",
    "@typescript-eslint/explicit-function-return-type": "warn",
    "@typescript-eslint/no-non-null-assertion": "warn",
    "@typescript-eslint/strict-boolean-expressions": "warn",
    "no-console": "warn"
  }
}
```

### Prettier Configuration

```json
{
  "semi": true,
  "trailingComma": "es5",
  "singleQuote": true,
  "printWidth": 100,
  "tabWidth": 2,
  "arrowParens": "always"
}
```

### TypeScript Configuration

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "lib": ["ES2022"],
    "moduleResolution": "node",
    "esModuleInterop": true,
    "strict": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true,
    "resolveJsonModule": true,
    "declaration": true,
    "declarationMap": true,
    "sourceMap": true,
    "outDir": "./dist",
    "rootDir": "./src",
    "baseUrl": ".",
    "paths": {
      "@/*": ["src/*"]
    },
    "noUnusedLocals": true,
    "noUnusedParameters": true,
    "noImplicitReturns": true,
    "noFallthroughCasesInSwitch": true
  },
  "include": ["src/**/*"],
  "exclude": ["node_modules", "dist"]
}
```

### Recommended Packages

```json
{
  "devDependencies": {
    "@typescript-eslint/eslint-plugin": "^7.0.0",
    "@typescript-eslint/parser": "^7.0.0",
    "eslint": "^8.57.0",
    "prettier": "^3.2.0",
    "typescript": "^5.3.0",
    "tsx": "^4.7.0",
    "vitest": "^1.2.0"
  }
}
```

---

## Common Patterns

### Singleton Pattern

```typescript
// Good: Class-based singleton
class Database {
  private static instance: Database;
  private connection: Connection;

  private constructor() {
    this.connection = createConnection();
  }

  static getInstance(): Database {
    if (!Database.instance) {
      Database.instance = new Database();
    }
    return Database.instance;
  }

  query(sql: string, params: unknown[]): Promise<QueryResult> {
    return this.connection.query(sql, params);
  }
}

// Usage
const db = Database.getInstance();
```

### Factory Pattern

```typescript
// Good: Factory functions for object creation
interface UserFactoryOptions {
  id: string;
  name: string;
  email: string;
  role?: UserRole;
}

function createUser(options: UserFactoryOptions): User {
  return {
    id: options.id,
    name: options.name,
    email: options.email,
    role: options.role || 'user',
    createdAt: new Date(),
    updatedAt: new Date(),
  };
}
```

### Builder Pattern

```typescript
// Good: Builder for complex object construction
class UserBuilder {
  private user: Partial<User> = {};

  withId(id: string): this {
    this.user.id = id;
    return this;
  }

  withName(name: string): this {
    this.user.name = name;
    return this;
  }

  withEmail(email: string): this {
    this.user.email = email;
    return this;
  }

  withRole(role: UserRole): this {
    this.user.role = role;
    return this;
  }

  build(): User {
    if (!this.user.id || !this.user.name || !this.user.email) {
      throw new Error('Missing required fields');
    }
    return {
      id: this.user.id,
      name: this.user.name,
      email: this.user.email,
      role: this.user.role || 'user',
      createdAt: new Date(),
      updatedAt: new Date(),
    };
  }
}

// Usage
const user = new UserBuilder()
  .withId('123')
  .withName('John Doe')
  .withEmail('john@example.com')
  .withRole('admin')
  .build();
```

### Dependency Injection

```typescript
// Good: Constructor injection for testability
class UserService {
  constructor(
    private readonly userRepository: UserRepository,
    private readonly emailService: EmailService,
    private readonly logger: Logger
  ) {}

  async createUser(data: CreateUserDto): Promise<User> {
    const user = await this.userRepository.create(data);
    await this.emailService.sendWelcomeEmail(user.email);
    this.logger.info(`User created: ${user.id}`);
    return user;
  }
}

// Usage
const userService = new UserRepository(
  new UserRepository(),
  new EmailService(),
  new ConsoleLogger()
);
```

---

## Anti-Patterns to Avoid

### Don't Use `any`

```typescript
// Bad: Using 'any' defeats the purpose of TypeScript
function process(data: any): any {
  return data.value;
}

// Good: Use specific types or generics
function process<T extends { value: unknown }>(data: T): T['value'] {
  return data.value;
}
```

### Don't Use Non-Null Assertions Excessively

```typescript
// Bad: Excessive use of ! operator
function getUserEmail(user: User | null): string {
  return user!.email!; // Could crash at runtime
}

// Good: Proper null checks
function getUserEmail(user: User | null): string {
  if (!user?.email) {
    throw new Error('User email is required');
  }
  return user.email;
}
```

### Don't Use Enums for String Constants

```typescript
// Avoid: String enums (prefer union types)
enum UserRole {
  ADMIN = 'admin',
  USER = 'user',
}

// Good: Use union types for simple string constants
type UserRole = 'admin' | 'user';

// Good: Use const enums or as const for objects
const UserRole = {
  ADMIN: 'admin',
  USER: 'user',
} as const;

type UserRole = typeof UserRole[keyof typeof UserRole];
```

### Don't Use Nested Ternaries

```typescript
// Bad: Hard to read
const result = condition1 ? value1 : condition2 ? value2 : condition3 ? value3 : defaultValue;

// Good: Use if-else or early returns
let result: string;
if (condition1) {
  result = value1;
} else if (condition2) {
  result = value2;
} else if (condition3) {
  result = value3;
} else {
  result = defaultValue;
}
```

### Don't Export Default

```typescript
// Bad: Default exports make refactoring harder
export default class UserService {
  // ...
}

// Good: Named exports are easier to maintain
export class UserService {
  // ...
}
```

---

## Additional Resources

- [TypeScript Handbook](https://www.typescriptlang.org/docs/handbook/intro.html)
- [Google TypeScript Style Guide](https://google.github.io/styleguide/tsguide.html)
- [TypeScript ESLint Rules](https://typescript-eslint.io/rules/)
- [Effective TypeScript](https://effectivetypescript.com/)
- [TypeScript Deep Dive](https://basarat.gitbook.io/typescript/)
