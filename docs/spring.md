# Spring: beans, injection points and slices down to the data

A Java service rarely calls its collaborators by their class: it calls an interface, and
Spring decides at startup which bean runs. Cartograph reads the same annotations Spring
does, so `callers` can say which beans a call through an interface reaches, and `path`
can follow a request past the HTTP client down to the collection or table it writes.

## What it reads

Annotations in the Java source, written with their package or imported from it. A
namesake from another framework (`com.other.di.Component`) is not Spring's and is
ignored. Both namespaces of the standards are read alike, so Spring Boot 2 (`javax.*`)
and Spring Boot 3 (`jakarta.*`) code index the same way.

| What | How it is written |
|---|---|
| Beans | a class annotated `@Component`, `@Service`, `@Repository`, `@Controller`, `@RestController` or `@Configuration`; the return type of a `@Bean` method |
| Bean names | `@Service("orders")`, `@Bean(name = "orders")`, `@Bean({"orders", "alias"})`, literals or constants (`Names.ORDERS`); else the class name decapitalized, with the classes around a nested one (`outer.Inner`) and `Impl` for a MapStruct mapper (`OrderService` → `orderService`) or the `@Bean` method's name |
| `@Primary`, `@Qualifier("x")` | on the bean class or the `@Bean` method |
| Injection points | fields with `@Autowired`, `@Inject` (`javax.inject` / `jakarta.inject`) or `@Resource` (`javax.annotation` / `jakarta.annotation`); the parameters of the constructor Spring calls: the one marked `@Autowired` or `@Inject`, else the only constructor of a bean class (the default since Spring 4.3), else the fields a Lombok `@RequiredArgsConstructor` (final fields without a value) or `@AllArgsConstructor` writes the constructor for; the parameters of `@Autowired` setters and methods and of `@Bean` methods |
| What an injection point receives | `T`, `List<T>`, `Set<T>`, `Queue<T>`, `Deque<T>`, `Collection<T>`, `Iterable<T>` and their usual implementations, `Map<String, T>` (and `ConcurrentMap`…), `T[]`, `Optional<T>`, `ObjectProvider<T>`, `Provider<T>`; `@Qualifier("x")`, `@Named("x")`, `@Resource`, `@Resource(name = "x")` |
| Data | `@Document(collection = "x")` / `@Document("x")` (Mongo; without a name, the class name decapitalized), `@Entity` / `@Table(name = "x")` (JPA), Spring Data's own `@Table("x")` (JDBC, Cassandra; a table without a name takes the class name in snake case, in lower case for Cassandra); Spring Data repositories (`extends MongoRepository<E, Id>`, `JpaRepository`, `CassandraRepository`, `R2dbcRepository`, `CrudRepository`…) and any call on one, whose interface is a bean; `getCollection(NAME[, E.class])` with a literal or a constant, in a file that imports the Mongo driver or Spring Data Mongo, `mongoTemplate.find(q, E.class)` and the other `MongoTemplate` / `MongoOperations` calls that name an entity class or a collection, `entityManager.find(E.class, id)` |

`@PostConstruct` (`javax.annotation` or `jakarta.annotation`) injects nothing: such a method
is read like any other, so its calls on injected fields resolve.

Which beans an injection point receives follows Spring's rules: every bean of the type
(its own beans and those of its subtypes, through `extends` and `implements`) for a list,
a set, a collection, a map or an array; for one bean, those a qualifier names, else the
only one, else the `@Primary` one, else the one named like the field or parameter; with
none of these, the point is reported as ambiguous. A `@Resource` goes by name first:
`@Resource(name = "x")` takes the bean of that name (or alias) and no other;
without a name it takes the bean named like the field or the setter's property
(`setWire` → `wire`), and only when none of the type is called so does it go by type
(`@Primary` included). When its name (an expression, a `lookup`), or the name of a bean
that fits, is not known, it narrows nothing. Where the field holds beans, a name that is
given stays (`Optional<T>`, `ObjectProvider<T>`) or names a collection whose elements are
not known (`List<T>`); a default name is the holder's and says nothing. `type` is no name. A qualifier written as an expression
whose value is not known narrows nothing (`qualified by a name not known`). An
`ObjectProvider` gives one bean, or all of them where it is streamed or looped over.

Calls are typed through what is injected: `service.create(dto)` on a field or a parameter
typed with an interface resolves to the interface method, and so does a call in a stream
lambda or loop over an injected list (`rules.stream().flatMap(r -> r.discounts().stream())`,
`rules.forEach(r -> …)`, `for (var r : rules)`, `rules.get(0).m()`, `map.values()…`,
`provider.getObject().m()`).

## What a call may run

Flows, slices and `callers` share one rule: what Spring chooses is annotated, never
subtracted. A call names the method its receiver's type has or inherits. `super.m()` (and
`Type.super.m()`), a private and a static method run as they are (a `super.m()` whose line has another call to
an `m` that did not resolve is taken as any other). Any other call may run:

- the method it names, kept whenever it has a body; one without a body (abstract, or an
  interface's) gives its place to what the beans run, when there are any;
- what the beans of the receiver's type and below have or inherit under that name (with
  one of its numbers of parameters; a class's method before an interface's default): first
  the beans Spring injects where the call is made, then the others, marked as such
  ("another Spring bean"). A slice follows those only once the rest is walked,
  and marks the data only they reach (`audit (Mongo, another bean)`).

A flow that gets to a method without a body other than by a call (it starts there, or a
route is declared on it) goes on to its beans.

## Where it shows up

- `callers` of an interface method: the call sites, then `Spring: N beans of T` and each
  injection point of `T` (those of the calling classes first) with the beans it receives:
  `injected into PricingEngine (constructor, List<PriceRule>) path:19: 3 beans: CouponRule,
  SeasonRule, VolumeRule`, `the same 3 beans` for the next one, `CardGateway (@Primary, of
  2)` or `WireGateway (qualified)` for a single bean.
- `callers` of a method that others declare: every call through those declarations, with
  what Spring makes of it: `via PaymentGateway.charge (Spring bean)` where a bean it
  injects runs the method, `(Spring injects CardGateway here (@Primary))` where it injects
  others. Nothing is noted where that is not known: no bean of the type in the source, a
  class only a `@Bean` method makes, or a receiver that is not what Spring injects
  (another parameter, a local variable). The receiver is followed from a constructor or
  setter parameter into the field it is assigned to (`this.x = x`, `requireNonNull(x)`,
  `List.copyOf(x)`, `new ArrayList<>(x)`; not when the parameter or the field is given
  another value), and from a collection into the loops and lambdas over it.
- `path` to a client that calls out over HTTP or IPC (see [mcp.md](mcp.md#slices)): past
  the controller, a call to an interface method goes on to the bean Spring injects there,
  down to the method that reads or writes, shown as `→ collection orders (Mongo, via
  Order)`.
- Flows (web map): a call goes on to the methods its beans run, dashed ("bean de Spring").

## What it does not read

- Beans that come from jars: Spring Boot auto-configuration, starters and libraries
  declare beans the repository's source never shows, so an injection point whose only
  beans come from there has `no bean of the type in the repo`.
- XML configuration (`applicationContext.xml`, `<bean>`), functional registration
  (`GenericApplicationContext.registerBean`, `BeanDefinitionRegistryPostProcessor`), the
  Kotlin bean DSL (`beans { … }`) and Kotlin sources in general.
- Profiles and conditions (`@Profile`, `@ConditionalOn…`): every bean counts, whichever
  profile is active.
- Component scanning limits (`@ComponentScan` base packages, filters): every annotated
  class of the repository is a bean.
- Calls on a variable of any other generic type (`Handler<Order> h; h.handle(o)`) are not
  typed, as before; its injection point is still listed.
- The table of an entity without an annotation in a repository whose interface does not say
  its store (`CrudRepository` in Spring Data JDBC): `R2dbcRepository`, `CassandraRepository`
  and `MongoRepository` give the store's default name.
- Overloads with the same number of parameters are not told apart, and what Spring injects
  is known by line: two calls to one method on one line are given the same note.
- Types written with their outer class (`Outer.Inner x`) or imported from inside one
  (`import a.Outer.Inner`): as receivers they are not typed.
- Generic type arguments of beans (`Handler<Order>` against `Handler<Invoice>`): beans are
  matched by their raw type.
- Collections named at run time, JDBC SQL strings and `@Query` texts.
