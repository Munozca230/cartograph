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
| Bean names | `@Service("orders")`, `@Bean(name = "orders")`, `@Bean({"orders", "alias"})`, literals or constants (`Names.ORDERS`); else the class name decapitalized (`OrderService` → `orderService`) or the `@Bean` method's name |
| `@Primary`, `@Qualifier("x")` | on the bean class or the `@Bean` method |
| Injection points | fields with `@Autowired`, `@Inject` (`javax.inject` / `jakarta.inject`) or `@Resource` (`javax.annotation` / `jakarta.annotation`); the parameters of the constructor Spring calls: the one marked `@Autowired` or `@Inject`, else the only constructor of a bean class (the default since Spring 4.3), else the fields a Lombok `@RequiredArgsConstructor` (final fields without a value) or `@AllArgsConstructor` writes the constructor for; the parameters of `@Autowired` setters and methods and of `@Bean` methods |
| What an injection point receives | `T`, `List<T>`, `Set<T>`, `Queue<T>`, `Deque<T>`, `Collection<T>`, `Iterable<T>` and their usual implementations, `Map<String, T>` (and `ConcurrentMap`…), `T[]`, `Optional<T>`, `ObjectProvider<T>`, `Provider<T>`; `@Qualifier("x")`, `@Named("x")`, `@Resource(name = "x")` |
| Data | `@Document(collection = "x")` / `@Document("x")` (Mongo; without a name, the class name decapitalized), `@Entity` / `@Table(name = "x")` (JPA), Spring Data's own `@Table("x")` (JDBC, Cassandra; a table without a name takes the class name in snake case, in lower case for Cassandra); Spring Data repositories (`extends MongoRepository<E, Id>`, `JpaRepository`, `CrudRepository`…) and any call on one, whose interface is a bean; `getCollection(NAME[, E.class])` with a literal or a constant, in a file that imports the Mongo driver or Spring Data Mongo, `mongoTemplate.find(q, E.class)` and the other `MongoTemplate` / `MongoOperations` calls that name an entity class or a collection, `entityManager.find(E.class, id)` |

`@PostConstruct` (`javax.annotation` or `jakarta.annotation`) injects nothing: such a method
is read like any other, so its calls on injected fields resolve.

Which beans an injection point receives follows Spring's rules: every bean of the type
(its own beans and those of its subtypes, through `extends` and `implements`) for a list,
a set, a collection, a map or an array; for one bean, those a qualifier names, else the
only one, else the `@Primary` one, else the one named like the field or parameter; with
none of these, the point is reported as ambiguous. A qualifier written as an expression
whose value is not known narrows nothing (`qualified by a name not known`). An
`ObjectProvider` gives one bean, or all of them where it is streamed or looped over.

Calls are typed through what is injected: `service.create(dto)` on a field or a parameter
typed with an interface resolves to the interface method, and so does a call in a stream
lambda or loop over an injected list (`rules.stream().flatMap(r -> r.discounts().stream())`,
`rules.forEach(r -> …)`, `for (var r : rules)`, `rules.get(0).m()`, `map.values()…`,
`provider.getObject().m()`).

## Where it shows up

- `callers` of an interface method: the call sites, then `Spring: N beans of T` and each
  injection point of `T` (those of the calling classes first) with the beans it receives:
  `injected into PricingEngine (constructor, List<PriceRule>) path:19: 3 beans: CouponRule,
  SeasonRule, VolumeRule`, `the same 3 beans` for the next one, `CardGateway (@Primary, of
  2)` or `WireGateway (qualified)` for a single bean.
- `callers` of a bean's method: the calls through its interface that reach it are marked
  `via PaymentGateway.charge (Spring bean)`; a call whose injected field or parameter
  receives another bean (another qualifier, another `@Primary`) is left out and counted
  (`1 call through PaymentGateway.charge reaches another bean`). A class that implements
  the interface but is no bean (a test's fake, one built with `new`) keeps its calls as
  plain supertype calls, and so does a call where Spring's choice is not in the source (no
  bean of the type, or a `@Bean` method that returns the interface) or whose receiver is
  not what Spring injects (another parameter, a local variable). The receiver is followed
  from a constructor or setter parameter into the field it is assigned to
  (`this.x = x`, `requireNonNull(x)`, `List.copyOf(x)`, `new ArrayList<>(x)`), and from a
  collection into the loops and lambdas over it.
- `path` to a client that calls out over HTTP or IPC (see [mcp.md](mcp.md#slices)): past
  the controller, a call to an interface method goes on to the bean Spring injects there,
  down to the method that reads or writes, shown as `→ collection orders (Mongo, via
  Order)`.
- Flows (web map): a call to an interface method continues to the beans that implement it.

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
- Generic type arguments of beans (`Handler<Order>` against `Handler<Invoice>`): beans are
  matched by their raw type.
- Collections named at run time, JDBC SQL strings and `@Query` texts.
